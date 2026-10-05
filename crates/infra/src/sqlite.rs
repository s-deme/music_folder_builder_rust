use music_folder_core::{
    ports::{
        ApplyStore, PlanBuildStore, PlanMusicAnchor, PlanRevisionBuild, PlanRevisionStore,
        PlanStore, PreflightStore, RecoveryItem, RecoveryStore, RollbackCompletionEvidence,
        RollbackStore, ScanStore, StagedPlanItem, VerifyStore,
    },
    require_current_naming_rules_schema, validate_apply_item_target, validate_naming_rules,
    validate_plan_item_for_persistence, windows_path_key, ApplyItem, ExecutionDisposition,
    FileFingerprint, FileKind, JournalState, MutationDirection, MutationLease, MutationStrategy,
    NamingRules, OperationAction, OperationIntent, OperationLog, OperationResult, PlanAction,
    PlanIssue, PlanItem, PreflightKind, PreflightLog, PreflightMode, PreflightOutcome,
    RecoveryAction, Risk, RunStatus, SafeTargetPath, ScannedFile, StagedFile, TrackMetadata,
    VerifyItem, VerifySubject, NAMING_RULES_SCHEMA_VERSION, PLAN_ISSUES_SCHEMA_VERSION,
    PLAN_SNAPSHOT_VERSION, WINDOWS_PATH_KEY_VERSION,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::{
    path_codec::{decode_path, encode_path, path_envelope},
    root_lock::RootProcessLock,
};

mod rows;
pub use rows::*;
mod archive;
mod diagnostics;
mod doctor;
mod migrations;

const METADATA_READER_ID: &str = "lofty";
const METADATA_READER_VERSION: &str = "lofty-v2";
const METADATA_SCHEMA_VERSION: i64 = 2;
const METADATA_READER_CONFIG_HASH: &str = "lofty-config-v1";
const METADATA_FINGERPRINT_ALGORITHM: &str = "sha256-v1";
const MUTATION_LEASE_TTL_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedPlanEnvelope {
    item: PlanItem,
    parent_plan_item_id: Option<String>,
    change_origin: Option<String>,
    change_reason: Option<String>,
}

type EncodedStagedPlanItem = (String, String, Vec<u8>, Option<String>, Option<Vec<u8>>);

fn encode_staged_plan_item(staged: &StagedPlanItem) -> Result<EncodedStagedPlanItem, String> {
    let (source_encoding, source_blob) = encode_path(&staged.item.file.path);
    let (target_encoding, target_blob) = staged
        .item
        .target
        .as_deref()
        .map(encode_path)
        .map_or((None, None), |(encoding, blob)| {
            (Some(encoding), Some(blob))
        });
    let mut item = staged.item.clone();
    item.file.path = PathBuf::new();
    item.target = None;
    let draft_json = serde_json::to_string(&StagedPlanEnvelope {
        item,
        parent_plan_item_id: staged.parent_plan_item_id.clone(),
        change_origin: staged.change_origin.clone(),
        change_reason: staged.change_reason.clone(),
    })
    .map_err(|error| error.to_string())?;
    Ok((
        draft_json,
        source_encoding.to_owned(),
        source_blob,
        target_encoding.map(str::to_owned),
        target_blob,
    ))
}

fn decode_staged_plan_item(
    draft_json: &str,
    source_encoding: &str,
    source_blob: &[u8],
    target_encoding: Option<&str>,
    target_blob: Option<&[u8]>,
) -> Result<StagedPlanItem, String> {
    let mut envelope: StagedPlanEnvelope =
        serde_json::from_str(draft_json).map_err(|error| error.to_string())?;
    envelope.item.file.path = decode_path(source_encoding, source_blob)?;
    envelope.item.target = match (target_encoding, target_blob) {
        (Some(encoding), Some(blob)) => Some(decode_path(encoding, blob)?),
        (None, None) => None,
        _ => return Err("plan_staging_target_encoding_incomplete".into()),
    };
    Ok(StagedPlanItem {
        item: envelope.item,
        parent_plan_item_id: envelope.parent_plan_item_id,
        change_origin: envelope.change_origin,
        change_reason: envelope.change_reason,
    })
}

fn load_staged_plan_page_from_connection(
    connection: &Connection,
    build_id: &str,
    after_ordinal: u64,
    limit: usize,
) -> Result<Vec<StagedPlanItem>, String> {
    let mut statement = connection
        .prepare(
            "SELECT draft_json,source_path_encoding,source_path_blob,
                    target_path_encoding,target_path_blob
               FROM plan_item_staging
              WHERE build_id=?1 AND ordinal>?2
              ORDER BY ordinal
              LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![
                build_id,
                i64::try_from(after_ordinal).map_err(|_| "plan_ordinal_invalid")?,
                i64::try_from(limit.max(1)).map_err(|_| "plan_page_limit_invalid")?
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    rows.into_iter()
        .map(
            |(draft, source_encoding, source_blob, target_encoding, target_blob)| {
                decode_staged_plan_item(
                    &draft,
                    &source_encoding,
                    &source_blob,
                    target_encoding.as_deref(),
                    target_blob.as_deref(),
                )
            },
        )
        .collect()
}

fn staging_target_key(item: &PlanItem) -> Option<String> {
    if item.action == PlanAction::Move
        || (item.risk == Risk::Conflict && item.reason.as_deref() == Some("target_conflict"))
    {
        item.target.as_deref().map(windows_path_key)
    } else {
        None
    }
}

pub struct SqliteScanStore {
    connection: Mutex<Connection>,
    database_path: PathBuf,
    lease_handles: Mutex<HashMap<u64, LeaseHandle>>,
}

struct LeaseHandle {
    _process_lock: RootProcessLock,
    ownership: LeaseOwnership,
}

#[derive(Clone)]
struct LeaseOwnership {
    owner_id: String,
    scope_keys: Vec<String>,
}

impl PreflightStore for SqliteScanStore {
    fn begin_preflight(
        &self,
        parent_attempt_id: &str,
        kind: PreflightKind,
        mode: PreflightMode,
    ) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let parent: Option<(String, String)> = transaction
            .query_row(
                "SELECT kind,status FROM workflow_attempts WHERE id=?1",
                params![parent_attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let (parent_kind, parent_status) =
            parent.ok_or_else(|| "preflight_parent_attempt_not_found".to_string())?;
        if parent_status != "running" {
            return Err("preflight_parent_attempt_not_running".into());
        }
        let expected_parent_kind = match (kind, mode) {
            (PreflightKind::Apply, PreflightMode::DryRun) => "dry_run",
            (PreflightKind::Apply, PreflightMode::Mutation) => "apply",
            (PreflightKind::Rollback, PreflightMode::DryRun) => "rollback_dry_run",
            (PreflightKind::Rollback, PreflightMode::Mutation) => "rollback",
            (PreflightKind::Recovery, PreflightMode::DryRun) => "recovery_dry_run",
            (PreflightKind::Recovery, PreflightMode::Mutation) => "recovery",
        };
        if parent_kind != expected_parent_kind {
            return Err("preflight_parent_attempt_kind_mismatch".into());
        }
        transaction
            .execute(
                "INSERT INTO preflight_runs(
                     id,parent_attempt_id,kind,mode,status,started_at
                 ) VALUES(?1,?2,?3,?4,'running',?5)",
                params![id, parent_attempt_id, kind.as_str(), mode.as_str(), now()],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn save_preflight_log(
        &self,
        preflight_id: &str,
        ordinal: u64,
        log: &PreflightLog,
    ) -> Result<(), String> {
        let (source_encoding, source_blob) = encode_path(&log.source);
        let (target_encoding, target_blob) = log
            .target
            .as_deref()
            .map(encode_path)
            .map_or((None, None), |(encoding, blob)| {
                (Some(encoding), Some(blob))
            });
        let expected = log.expected.as_ref();
        let observed = log.observed.as_ref();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let changed = connection
            .execute(
                "INSERT INTO preflight_logs(
                     preflight_id,ordinal,item_id,
                     source_path_encoding,source_path_blob,
                     target_path_encoding,target_path_blob,outcome,code,
                     expected_size,expected_mtime_ns,expected_content_sha256,
                     expected_file_identity,expected_fingerprint_version,
                     observed_size,observed_mtime_ns,observed_content_sha256,
                     observed_file_identity,observed_fingerprint_version,observed_at
                 )
                 SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                        ?15,?16,?17,?18,?19,?20
                  WHERE EXISTS(SELECT 1 FROM preflight_runs
                                WHERE id=?1 AND status='running')",
                params![
                    preflight_id,
                    i64::try_from(ordinal).map_err(|_| "preflight_ordinal_invalid")?,
                    log.item_id,
                    source_encoding,
                    source_blob,
                    target_encoding,
                    target_blob,
                    log.outcome.as_str(),
                    log.code,
                    expected.map(|value| value.size_bytes as i64),
                    expected.map(|value| value.mtime_ns.to_string()),
                    expected.and_then(|value| value.content_sha256.as_deref()),
                    expected.and_then(|value| value.file_identity.as_deref()),
                    expected.map(|value| i64::from(value.version)),
                    observed.map(|value| value.size_bytes as i64),
                    observed.map(|value| value.mtime_ns.to_string()),
                    observed.and_then(|value| value.content_sha256.as_deref()),
                    observed.and_then(|value| value.file_identity.as_deref()),
                    observed.map(|value| i64::from(value.version)),
                    now(),
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed == 1 {
            if log.outcome != PreflightOutcome::Passed {
                let context: Option<(String, String)> = connection
                    .query_row(
                        "SELECT preflight.parent_attempt_id,attempt.logical_run_id
                           FROM preflight_runs preflight
                           JOIN workflow_attempts attempt
                             ON attempt.id=preflight.parent_attempt_id
                          WHERE preflight.id=?1",
                        params![preflight_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .unwrap_or(None);
                if let Some((attempt_id, logical_run_id)) = context {
                    let raw_code = log.code.as_deref().unwrap_or("preflight_rejected");
                    let code = stable_diagnostic_code(raw_code, "preflight_rejected");
                    let security = security_rejection_code(&code);
                    diagnostics::insert_diagnostic_best_effort(
                        &connection,
                        DiagnosticEventInput {
                            run_id: Some(logical_run_id),
                            attempt_id: Some(attempt_id),
                            class: if security { "audit" } else { "diagnostic" }.into(),
                            severity: "warning".into(),
                            phase: "preflight".into(),
                            code,
                            item_id: Some(log.item_id.clone()),
                            sequence_no: Some(ordinal),
                            path_role: Some(if log.target.is_some() {
                                "target".into()
                            } else {
                                "source".into()
                            }),
                            message_key: if security {
                                "security_rejected".into()
                            } else {
                                "preflight_rejected".into()
                            },
                            payload: serde_json::json!({
                                "outcome": log.outcome.as_str(),
                                "cause_chain": diagnostic_cause_chain(raw_code),
                                "expected_available": log.expected.is_some(),
                                "observed_available": log.observed.is_some(),
                            }),
                            contains_sensitive_path: false,
                            protected: security,
                        },
                    );
                }
            }
            Ok(())
        } else {
            Err("preflight_log_rejected".into())
        }
    }

    fn finish_preflight(
        &self,
        preflight_id: &str,
        outcome: PreflightOutcome,
        passed: u64,
        blocked: u64,
    ) -> Result<(), String> {
        let changed = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .execute(
                "UPDATE preflight_runs
                    SET status=?2,finished_at=?3,passed_count=?4,blocked_count=?5
                  WHERE id=?1 AND status='running'",
                params![
                    preflight_id,
                    outcome.as_str(),
                    now(),
                    i64::try_from(passed).map_err(|_| "preflight_count_invalid")?,
                    i64::try_from(blocked).map_err(|_| "preflight_count_invalid")?,
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed == 1 {
            Ok(())
        } else {
            Err("preflight_not_running".into())
        }
    }
}

struct ManualTargetPlanContext {
    status: String,
    archived_at: Option<i64>,
    root_encoding: String,
    root_blob: Vec<u8>,
    rules_json: String,
    rules_schema_version: i64,
    snapshot_version: i64,
    path_policy_version: i64,
    item_exists: bool,
}

fn load_lease_scopes(connection: &Connection, current_time: i64) -> Result<Vec<PathBuf>, String> {
    let mut statement = connection
        .prepare(
            "SELECT scope_path_encoding,scope_path_blob
               FROM mutation_leases
              WHERE expires_at>?1
              ORDER BY scope_key",
        )
        .map_err(|error| error.to_string())?;
    let encoded = statement
        .query_map(params![current_time], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    encoded
        .into_iter()
        .map(|(encoding, raw)| decode_path(&encoding, &raw))
        .collect()
}

fn expired_overlapping_lease_tokens(
    connection: &Connection,
    requested_scopes: &[PathBuf],
    current_time: i64,
) -> Result<Vec<i64>, String> {
    let mut statement = connection
        .prepare(
            "SELECT fencing_token,scope_path_encoding,scope_path_blob
               FROM mutation_leases
              WHERE expires_at<=?1
              ORDER BY fencing_token,scope_key",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![current_time], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut tokens = Vec::new();
    for (token, encoding, raw) in rows {
        let scope = decode_path(&encoding, &raw)?;
        if requested_scopes
            .iter()
            .any(|requested| music_folder_core::windows_path_scopes_overlap(&scope, requested))
        {
            tokens.push(token);
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    Ok(tokens)
}

fn renew_complete_mutation_lease(
    transaction: &Transaction<'_>,
    fencing_token: u64,
    ownership: &LeaseOwnership,
    current_time: i64,
) -> Result<(), String> {
    let fencing_token =
        i64::try_from(fencing_token).map_err(|_| "mutation_fencing_token_invalid")?;
    let persisted = {
        let mut statement = transaction
            .prepare(
                "SELECT scope_key,owner_id
                   FROM mutation_leases
                  WHERE fencing_token=?1
                  ORDER BY scope_key",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![fencing_token], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    if persisted.len() != ownership.scope_keys.len()
        || persisted
            .iter()
            .any(|(_, owner_id)| owner_id != &ownership.owner_id)
        || persisted
            .iter()
            .map(|(scope_key, _)| scope_key)
            .ne(ownership.scope_keys.iter())
    {
        return Err("mutation_lease_stale".into());
    }
    let changed = transaction
        .execute(
            "UPDATE mutation_leases
                SET heartbeat_at=?3,expires_at=?4
              WHERE owner_id=?1 AND fencing_token=?2",
            params![
                ownership.owner_id,
                fencing_token,
                current_time,
                current_time + MUTATION_LEASE_TTL_SECONDS
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == ownership.scope_keys.len() {
        Ok(())
    } else {
        Err("mutation_lease_stale".into())
    }
}

fn unresolved_journal_overlaps(
    connection: &Connection,
    scopes: &[PathBuf],
    recovery_operation_id: Option<&str>,
    current_time: i64,
) -> Result<bool, String> {
    let mut statement = connection
        .prepare(
            "SELECT journal.id,
                    journal.source_path_encoding,journal.source_path_blob,
                    journal.target_path_encoding,journal.target_path_blob
               FROM operation_journal journal
              WHERE journal.state NOT IN ('completed','failed')
                AND (?1 IS NULL OR journal.id<>?1)
                AND NOT EXISTS (
                    SELECT 1 FROM mutation_leases lease
                     WHERE lease.fencing_token=journal.fencing_token
                       AND lease.expires_at>?2
                )
              ORDER BY journal.id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![recovery_operation_id, current_time], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    for (_, source_encoding, source_raw, target_encoding, target_raw) in rows {
        let source = decode_path(&source_encoding, &source_raw)?;
        let target = decode_path(&target_encoding, &target_raw)?;
        if scopes.iter().any(|scope| {
            music_folder_core::windows_path_scopes_overlap(scope, &source)
                || music_folder_core::windows_path_scopes_overlap(scope, &target)
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

impl SqliteScanStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA busy_timeout=5000;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS schema_migrations (
                     version INTEGER PRIMARY KEY,
                     applied_at INTEGER NOT NULL
                 );",
            )
            .map_err(|e| e.to_string())?;
        migrations::upgrade(&mut connection, now())?;
        reconcile_abandoned_journals(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            database_path: path.to_path_buf(),
            lease_handles: Mutex::new(HashMap::new()),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn mutation_roots_for_plan(&self, plan_id: &str) -> Result<Vec<PathBuf>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let encoded: Option<(String, Vec<u8>, String, Vec<u8>)> = conn
            .query_row(
                "SELECT scan.source_root_encoding,scan.source_root_blob,
                        plan.target_root_encoding,plan.target_root_blob
                   FROM plan_runs plan
                   JOIN scan_runs scan ON scan.id=plan.scan_id
                  WHERE plan.id=?1",
                params![plan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let (source_encoding, source_raw, target_encoding, target_raw) =
            encoded.ok_or_else(|| "plan_not_found".to_string())?;
        Ok(vec![
            decode_path(&source_encoding, &source_raw)?,
            decode_path(&target_encoding, &target_raw)?,
        ])
    }

    /// Rehydrates the persisted Plan boundary and issues only a Core-validated
    /// manual target. Desktop uses this before minting its one-shot IPC
    /// capability; revision still revalidates the same path inside Core.
    pub fn validate_manual_target_for_plan(
        &self,
        plan_id: &str,
        plan_item_id: &str,
        target: &Path,
    ) -> Result<PathBuf, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let persisted: Option<ManualTargetPlanContext> = conn
            .query_row(
                "SELECT plan.status,plan.archived_at,
                        plan.target_root_encoding,plan.target_root_blob,plan.rules_json,
                        plan.rules_schema_version,plan.snapshot_version,plan.path_policy_version,
                        EXISTS(SELECT 1 FROM plan_items item
                                WHERE item.plan_id=plan.id AND item.id=?2)
                   FROM plan_runs plan
                  WHERE plan.id=?1",
                params![plan_id, plan_item_id],
                |row| {
                    Ok(ManualTargetPlanContext {
                        status: row.get(0)?,
                        archived_at: row.get(1)?,
                        root_encoding: row.get(2)?,
                        root_blob: row.get(3)?,
                        rules_json: row.get(4)?,
                        rules_schema_version: row.get(5)?,
                        snapshot_version: row.get(6)?,
                        path_policy_version: row.get(7)?,
                        item_exists: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let context = persisted.ok_or_else(|| "plan_not_found".to_string())?;
        if context.status != "completed" || context.archived_at.is_some() {
            return Err("plan_not_revisable".into());
        }
        if !context.item_exists {
            return Err("plan_item_not_found".into());
        }
        if context.snapshot_version != i64::from(PLAN_SNAPSHOT_VERSION)
            || context.path_policy_version
                != i64::from(music_folder_core::WINDOWS_PATH_POLICY_VERSION)
        {
            return Err("legacy_plan_non_revisable".into());
        }
        let target_root = decode_path(&context.root_encoding, &context.root_blob)?;
        let naming = current_naming_rules(
            context.rules_schema_version,
            &context.rules_json,
            "legacy_plan_non_revisable",
        )?;
        match SafeTargetPath::from_manual_absolute_target(
            &target_root,
            target,
            naming.allow_long_paths,
        ) {
            Ok(validated) => Ok(validated.into_path_buf()),
            Err(error) => {
                let raw_error = error.to_string();
                let code = stable_diagnostic_code(&raw_error, "manual_target_rejected");
                diagnostics::insert_diagnostic_best_effort(
                    &conn,
                    DiagnosticEventInput {
                        run_id: Some(plan_id.to_owned()),
                        attempt_id: None,
                        class: "audit".into(),
                        severity: "warning".into(),
                        phase: "plan_revision".into(),
                        code,
                        item_id: Some(plan_item_id.to_owned()),
                        sequence_no: None,
                        path_role: Some("target".into()),
                        message_key: "security_rejected".into(),
                        payload: serde_json::json!({
                            "cause_chain": diagnostic_cause_chain(&raw_error),
                            "candidate_path_omitted": true,
                        }),
                        contains_sensitive_path: true,
                        protected: true,
                    },
                );
                Err(raw_error)
            }
        }
    }

    fn mutation_roots_for_execution(&self, execution_id: &str) -> Result<Vec<PathBuf>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let encoded: Option<(String, Vec<u8>, String, Vec<u8>)> = conn
            .query_row(
                "SELECT scan.source_root_encoding,scan.source_root_blob,
                        plan.target_root_encoding,plan.target_root_blob
                   FROM execution_runs execution
                   JOIN plan_runs plan ON plan.id=execution.plan_id
                   JOIN scan_runs scan ON scan.id=plan.scan_id
                  WHERE execution.id=?1",
                params![execution_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let (source_encoding, source_raw, target_encoding, target_raw) =
            encoded.ok_or_else(|| "execution_not_found".to_string())?;
        Ok(vec![
            decode_path(&source_encoding, &source_raw)?,
            decode_path(&target_encoding, &target_raw)?,
        ])
    }

    fn mutation_roots_for_operation(&self, operation_id: &str) -> Result<Vec<PathBuf>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let encoded: Option<(String, Vec<u8>, String, Vec<u8>)> = conn
            .query_row(
                "SELECT scan.source_root_encoding,scan.source_root_blob,
                        plan.target_root_encoding,plan.target_root_blob
                   FROM operation_journal journal
                   JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id
                   JOIN scan_runs scan ON scan.id=attempt.root_scan_id
                   JOIN plan_runs plan ON plan.scan_id=scan.id
                   LEFT JOIN execution_runs execution
                     ON execution.id=journal.attempt_id AND execution.plan_id=plan.id
                   LEFT JOIN rollback_runs rollback ON rollback.id=journal.attempt_id
                   LEFT JOIN execution_runs rollback_execution
                     ON rollback_execution.id=rollback.execution_id
                    AND rollback_execution.plan_id=plan.id
                  WHERE journal.id=?1
                    AND (execution.id IS NOT NULL OR rollback_execution.id IS NOT NULL)
                  ORDER BY plan.started_at DESC,plan.id DESC
                  LIMIT 1",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let (source_encoding, source_raw, target_encoding, target_raw) =
            encoded.ok_or_else(|| "recovery_operation_not_found".to_string())?;
        Ok(vec![
            decode_path(&source_encoding, &source_raw)?,
            decode_path(&target_encoding, &target_raw)?,
        ])
    }

    fn lease_ownership_for_token(&self, fencing_token: u64) -> Result<LeaseOwnership, String> {
        self.lease_handles
            .lock()
            .map_err(|_| "mutation_lease_handle_mutex_poisoned".to_string())?
            .get(&fencing_token)
            .map(|handle| handle.ownership.clone())
            .ok_or_else(|| "mutation_lease_stale".to_string())
    }

    fn lease_ownership(&self, lease: &MutationLease) -> Result<LeaseOwnership, String> {
        let ownership = self.lease_ownership_for_token(lease.fencing_token)?;
        if ownership.owner_id == lease.owner_id
            && ownership.scope_keys.first() == Some(&lease.scope_key)
        {
            Ok(ownership)
        } else {
            Err("mutation_lease_stale".into())
        }
    }

    fn acquire_mutation_lease(
        &self,
        scopes: &[PathBuf],
        owner_id: &str,
        recovery_operation_id: Option<&str>,
    ) -> Result<MutationLease, String> {
        let mut scopes = scopes.to_vec();
        scopes.sort_by_key(|scope| windows_path_key(scope));
        scopes.dedup_by(|left, right| windows_path_key(left) == windows_path_key(right));
        if scopes.is_empty() {
            return Err("mutation_scope_empty".into());
        }
        let process_lock = RootProcessLock::acquire_many(&scopes)?;
        let encoded_scopes = scopes
            .iter()
            .map(|scope| {
                let (encoding, raw) = encode_path(scope);
                (windows_path_key(scope), encoding.to_owned(), raw)
            })
            .collect::<Vec<_>>();
        let scope_key = encoded_scopes[0].0.clone();
        let current_time = now();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let active_scopes = load_lease_scopes(&transaction, current_time)?;
        if active_scopes.iter().any(|active| {
            scopes
                .iter()
                .any(|requested| music_folder_core::windows_path_scopes_overlap(active, requested))
        }) {
            return Err("mutation_scope_busy".into());
        }
        if unresolved_journal_overlaps(&transaction, &scopes, recovery_operation_id, current_time)?
        {
            return Err("mutation_recovery_required".into());
        }
        for token in expired_overlapping_lease_tokens(&transaction, &scopes, current_time)? {
            transaction
                .execute(
                    "DELETE FROM mutation_leases WHERE fencing_token=?1",
                    params![token],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute(
                "UPDATE mutation_lease_sequence
                    SET last_fencing_token=last_fencing_token+1 WHERE singleton=1",
                [],
            )
            .map_err(|error| error.to_string())?;
        let fencing_token: i64 = transaction
            .query_row(
                "SELECT last_fencing_token FROM mutation_lease_sequence WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        for (key, encoding, raw) in &encoded_scopes {
            transaction
                .execute(
                    "INSERT INTO mutation_leases(
                         scope_key,scope_path_encoding,scope_path_blob,owner_id,fencing_token,
                         heartbeat_at,expires_at
                     ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        key,
                        encoding,
                        raw,
                        owner_id,
                        fencing_token,
                        current_time,
                        current_time + MUTATION_LEASE_TTL_SECONDS
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())?;

        let fencing_token = u64::try_from(fencing_token)
            .map_err(|_| "mutation_fencing_token_invalid".to_string())?;
        let lease = MutationLease {
            owner_id: owner_id.to_owned(),
            scope_key,
            fencing_token,
        };
        let ownership = LeaseOwnership {
            owner_id: owner_id.to_owned(),
            scope_keys: encoded_scopes
                .iter()
                .map(|(key, _, _)| key.clone())
                .collect(),
        };
        let mut handles = self
            .lease_handles
            .lock()
            .map_err(|_| "mutation_lease_handle_mutex_poisoned".to_string())?;
        if handles
            .insert(
                fencing_token,
                LeaseHandle {
                    _process_lock: process_lock,
                    ownership,
                },
            )
            .is_some()
        {
            let _ = self.connection.lock().map(|connection| {
                connection.execute(
                    "DELETE FROM mutation_leases WHERE owner_id=?1 AND fencing_token=?2",
                    params![owner_id, fencing_token as i64],
                )
            });
            return Err("mutation_fencing_token_reused".into());
        }
        Ok(lease)
    }

    fn heartbeat_mutation_lease(&self, lease: &MutationLease) -> Result<(), String> {
        let current_time = now();
        let ownership = self.lease_ownership(lease)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        renew_complete_mutation_lease(&transaction, lease.fencing_token, &ownership, current_time)?;
        transaction.commit().map_err(|error| error.to_string())
    }

    fn release_mutation_lease(&self, lease: &MutationLease) -> Result<(), String> {
        let changed = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .execute(
                "DELETE FROM mutation_leases
                  WHERE owner_id=?2 AND fencing_token=?3",
                params![lease.scope_key, lease.owner_id, lease.fencing_token as i64],
            )
            .map_err(|error| error.to_string())?;
        let removed = self
            .lease_handles
            .lock()
            .map_err(|_| "mutation_lease_handle_mutex_poisoned".to_string())?
            .remove(&lease.fencing_token);
        if removed
            .as_ref()
            .is_some_and(|handle| changed == handle.ownership.scope_keys.len())
        {
            Ok(())
        } else {
            Err("mutation_lease_stale".into())
        }
    }

    fn prepare_journal_operation(&self, intent: &OperationIntent) -> Result<(), String> {
        let ownership = self
            .lease_ownership_for_token(intent.fencing_token)
            .map_err(|error| {
                if error == "mutation_lease_stale" {
                    "journal_prepare_rejected".to_string()
                } else {
                    error
                }
            })?;
        let (source_encoding, source_blob) = encode_path(&intent.source);
        let (target_encoding, target_blob) = encode_path(&intent.target);
        let (temporary_encoding, temporary_blob) = encode_path(&intent.temporary);
        let current_time = now();
        let payload = serde_json::json!({
            "event": "prepared",
            "direction": intent.direction.as_str(),
            "strategy": intent.strategy.as_str(),
            "expected_size": intent.expected_size,
            "has_expected_content_sha256": intent.expected_content_sha256.is_some(),
            "has_expected_file_identity": intent.expected_file_identity.is_some(),
        })
        .to_string();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        renew_complete_mutation_lease(&transaction, intent.fencing_token, &ownership, current_time)
            .map_err(|error| {
                if error == "mutation_lease_stale" {
                    "journal_prepare_rejected".to_string()
                } else {
                    error
                }
            })?;
        let changed = transaction
            .execute(
                "INSERT INTO operation_journal(
                     id,attempt_id,direction,strategy,plan_item_id,sequence_no,state,
                     source_path,source_path_encoding,source_path_blob,
                     target_path,target_path_encoding,target_path_blob,
                     temporary_path,temporary_path_encoding,temporary_path_blob,
                     expected_size,expected_content_sha256,expected_file_identity,
                     fencing_token,transition_no,created_at,updated_at
                 )
                 VALUES(?1,?2,?3,?4,?5,?6,'prepared',?7,?8,?9,?10,?11,?12,
                        ?13,?14,?15,?16,?17,?18,?19,0,?20,?20)",
                params![
                    intent.operation_id,
                    intent.attempt_id,
                    intent.direction.as_str(),
                    intent.strategy.as_str(),
                    intent.plan_item_id,
                    intent.sequence_no as i64,
                    intent.source.to_string_lossy(),
                    source_encoding,
                    source_blob,
                    intent.target.to_string_lossy(),
                    target_encoding,
                    target_blob,
                    intent.temporary.to_string_lossy(),
                    temporary_encoding,
                    temporary_blob,
                    intent.expected_size.map(|value| value as i64),
                    intent.expected_content_sha256,
                    intent.expected_file_identity,
                    intent.fencing_token as i64,
                    current_time
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("journal_prepare_rejected".into());
        }
        transaction
            .execute(
                "INSERT INTO operation_journal_events(
                     operation_id,transition_no,from_state,to_state,fencing_token,
                     payload_schema_version,payload_json,created_at
                 ) VALUES(?1,0,NULL,'prepared',?2,1,?3,?4)",
                params![
                    intent.operation_id,
                    i64::try_from(intent.fencing_token)
                        .map_err(|_| "mutation_fencing_token_invalid")?,
                    payload,
                    current_time
                ],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())
    }

    fn transition_journal_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        if !music_folder_core::journal_transition_allowed(from, to) {
            return Err(format!(
                "journal_transition_invalid:{}->{}",
                from.as_str(),
                to.as_str()
            ));
        }
        let ownership = self.lease_ownership(lease).map_err(|error| {
            if error == "mutation_lease_stale" {
                "journal_transition_rejected".to_string()
            } else {
                error
            }
        })?;
        let current_time = now();
        let payload = serde_json::json!({
            "event": "state_transition",
            "staged_size": staged.map(|value| value.size_bytes),
            "staged_content_sha256": staged.map(|value| value.content_sha256.as_str()),
            "staged_file_identity": staged.and_then(|value| value.file_identity.as_deref()),
            "error_code": error,
        })
        .to_string();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|database_error| database_error.to_string())?;
        renew_complete_mutation_lease(&transaction, lease.fencing_token, &ownership, current_time)
            .map_err(|error| {
                if error == "mutation_lease_stale" {
                    "journal_transition_rejected".to_string()
                } else {
                    error
                }
            })?;
        let changed = transaction
            .execute(
                "UPDATE operation_journal
                    SET state=?3,
                        staged_size=COALESCE(?5,staged_size),
                        staged_content_sha256=COALESCE(?6,staged_content_sha256),
                        staged_file_identity=COALESCE(?7,staged_file_identity),
                        error=?8,transition_no=transition_no+1,updated_at=?9
                  WHERE id=?1 AND state=?2 AND fencing_token=?4
                ",
                params![
                    operation_id,
                    from.as_str(),
                    to.as_str(),
                    lease.fencing_token as i64,
                    staged.map(|value| value.size_bytes as i64),
                    staged.map(|value| value.content_sha256.as_str()),
                    staged.and_then(|value| value.file_identity.as_deref()),
                    error,
                    current_time
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("journal_transition_rejected".into());
        }
        let transition_no: i64 = transaction
            .query_row(
                "SELECT transition_no FROM operation_journal WHERE id=?1",
                params![operation_id],
                |row| row.get(0),
            )
            .map_err(|database_error| database_error.to_string())?;
        transaction
            .execute(
                "INSERT INTO operation_journal_events(
                     operation_id,transition_no,from_state,to_state,fencing_token,
                     payload_schema_version,payload_json,created_at
                 ) VALUES(?1,?2,?3,?4,?5,1,?6,?7)",
                params![
                    operation_id,
                    transition_no,
                    from.as_str(),
                    to.as_str(),
                    i64::try_from(lease.fencing_token)
                        .map_err(|_| "mutation_fencing_token_invalid")?,
                    payload,
                    current_time
                ],
            )
            .map_err(|database_error| database_error.to_string())?;
        if to == JournalState::RecoveryRequired || error.is_some() {
            let context: Option<(String, String, String, String, i64, String)> = transaction
                .query_row(
                    "SELECT journal.attempt_id,attempt.logical_run_id,journal.direction,
                            journal.plan_item_id,journal.sequence_no,journal.strategy
                       FROM operation_journal journal
                       JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id
                      WHERE journal.id=?1",
                    params![operation_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()
                .unwrap_or(None);
            if let Some((attempt_id, logical_run_id, direction, item_id, sequence, strategy)) =
                context
            {
                let raw_error = error.unwrap_or("journal_recovery_required");
                diagnostics::insert_diagnostic_best_effort(
                    &transaction,
                    DiagnosticEventInput {
                        run_id: Some(logical_run_id),
                        attempt_id: Some(attempt_id),
                        class: if to == JournalState::RecoveryRequired {
                            "recovery".into()
                        } else {
                            "diagnostic".into()
                        },
                        severity: "error".into(),
                        phase: direction,
                        code: stable_diagnostic_code(raw_error, "journal_transition_failed"),
                        item_id: Some(item_id),
                        sequence_no: u64::try_from(sequence).ok(),
                        path_role: Some("target".into()),
                        message_key: if to == JournalState::RecoveryRequired {
                            "workflow_recovery_required".into()
                        } else {
                            "journal_transition_failed".into()
                        },
                        payload: serde_json::json!({
                            "operation_id": operation_id,
                            "from_state": from.as_str(),
                            "to_state": to.as_str(),
                            "strategy": strategy,
                            "cause_chain": diagnostic_cause_chain(raw_error),
                        }),
                        contains_sensitive_path: false,
                        protected: to == JournalState::RecoveryRequired,
                    },
                );
            }
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    fn load_successful_rollback_operations(
        &self,
        rollback_id: &str,
    ) -> Result<Vec<VerifyItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT operation.id,operation.sequence_no,operation.source_path_encoding,
                    operation.source_path_blob,operation.target_path_encoding,
                    operation.target_path_blob,operation.action,operation.expected_size,
                    operation.expected_content_sha256,operation.expected_file_identity
               FROM rollback_logs rollback_log
               JOIN operation_logs operation ON operation.id=rollback_log.operation_id
              WHERE rollback_log.rollback_run_id=?1 AND rollback_log.result='success'
              ORDER BY operation.sequence_no",
            )
            .map_err(|error| error.to_string())?;
        let items = statement
            .query_map(params![rollback_id], verify_item_from_row)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(items)
    }

    fn load_successful_recovery_operations(
        &self,
        recovery_id: &str,
    ) -> Result<Vec<VerifyItem>, String> {
        type RecoveryVerifyRow = (
            Option<String>,
            i64,
            String,
            Vec<u8>,
            String,
            Vec<u8>,
            Option<i64>,
            Option<String>,
            Option<String>,
            String,
            String,
        );
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let row: Option<RecoveryVerifyRow> = conn
            .query_row(
                "SELECT CASE journal.direction
                          WHEN 'apply' THEN (
                              SELECT operation.id FROM operation_logs operation
                               WHERE operation.execution_id=journal.attempt_id
                                 AND operation.sequence_no=journal.sequence_no
                               ORDER BY operation.id LIMIT 1
                          )
                          WHEN 'rollback' THEN (
                              SELECT rollback_log.operation_id
                                FROM rollback_logs rollback_log
                                JOIN operation_logs operation
                                  ON operation.id=rollback_log.operation_id
                               WHERE rollback_log.rollback_run_id=journal.attempt_id
                                 AND operation.sequence_no=journal.sequence_no
                               ORDER BY rollback_log.id LIMIT 1
                          )
                        END,
                        journal.sequence_no,
                        journal.source_path_encoding,journal.source_path_blob,
                        journal.target_path_encoding,journal.target_path_blob,
                        journal.expected_size,journal.expected_content_sha256,
                        journal.expected_file_identity,recovery.action,journal.strategy
                   FROM recovery_runs recovery
                   JOIN operation_journal journal ON journal.id=recovery.operation_id
                  WHERE recovery.id=?1 AND recovery.status='completed'",
                params![recovery_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        row.map(
            |(
                id,
                sequence,
                source_encoding,
                source_blob,
                target_encoding,
                target_blob,
                expected_size,
                expected_hash,
                expected_identity,
                recovery_action,
                strategy,
            )| {
                let action = match recovery_action.as_str() {
                    "resume" => match MutationStrategy::from_code(&strategy) {
                        Some(MutationStrategy::AtomicNoReplaceRename) => OperationAction::Move,
                        Some(MutationStrategy::CopyPublishDelete) => OperationAction::CopyDelete,
                        Some(MutationStrategy::ConditionalDeleteOnly) => OperationAction::Skip,
                        None => return Err(format!("journal_strategy_unknown:{strategy}")),
                    },
                    "rollback_published" => OperationAction::CopySourceRetained,
                    "discard_unpublished_temporary" => OperationAction::Skip,
                    "manual_intervention" => {
                        return Err("manual_recovery_is_not_verifiable".to_string())
                    }
                    value => return Err(format!("recovery_action_unknown:{value}")),
                };
                Ok(VerifyItem {
                    operation_id: id
                        .ok_or_else(|| "recovery_operation_log_not_found".to_string())?,
                    sequence_no: u64::try_from(sequence)
                        .map_err(|_| "journal_sequence_invalid".to_string())?,
                    source: decode_path(&source_encoding, &source_blob)?,
                    target: Some(decode_path(&target_encoding, &target_blob)?),
                    action,
                    expected_size: expected_size
                        .map(|size| {
                            u64::try_from(size)
                                .map_err(|_| "recovery_expected_size_invalid".to_string())
                        })
                        .transpose()?,
                    expected_content_sha256: expected_hash,
                    expected_file_identity: expected_identity,
                })
            },
        )
        .transpose()
        .map(|item| item.into_iter().collect())
    }

    pub fn list_history(&self, limit: u32, cursor: Option<i64>) -> Result<Vec<HistoryRow>, String> {
        self.list_history_filtered(limit, cursor, None, None, None, None, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_history_filtered(
        &self,
        limit: u32,
        cursor_started_at: Option<i64>,
        cursor_id: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        query: Option<&str>,
        oldest_first: bool,
    ) -> Result<Vec<HistoryRow>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let cursor_time =
            cursor_started_at.unwrap_or(if oldest_first { i64::MIN } else { i64::MAX });
        let cursor_key = cursor_id.unwrap_or(if oldest_first { "" } else { "\u{10ffff}" });
        let comparison = if oldest_first {
            "(started_at > ?4 OR (started_at = ?4 AND id > ?5))"
        } else {
            "(started_at < ?4 OR (started_at = ?4 AND id < ?5))"
        };
        let ordering = if oldest_first { "ASC" } else { "DESC" };
        let sql = format!(
            "WITH history AS (
             SELECT attempt.id,
                    CASE attempt.kind
                      WHEN 'plan_revision' THEN 'plan'
                      WHEN 'dry_run' THEN 'apply'
                      WHEN 'rollback_dry_run' THEN 'rollback'
                      WHEN 'recovery_dry_run' THEN 'recovery'
                      ELSE attempt.kind END kind,
                    CASE attempt.kind
                      WHEN 'plan_revision' THEN 'revision'
                      WHEN 'dry_run' THEN 'dry_run'
                      WHEN 'apply' THEN 'apply'
                      WHEN 'rollback_dry_run' THEN 'dry_run'
                      WHEN 'rollback' THEN 'rollback'
                      WHEN 'recovery_dry_run' THEN 'dry_run'
                      WHEN 'recovery' THEN 'recovery'
                    END mode,
                    attempt.logical_run_id,attempt.attempt_no,
                    attempt.predecessor_attempt_id,attempt.actor,
                    CASE WHEN scan.archived_at IS NOT NULL AND attempt.kind<>'archive'
                         THEN 'archived' ELSE attempt.status END status,
                    attempt.started_at,attempt.finished_at,
                    attempt.parent_attempt_id parent_id,attempt.root_scan_id,
                    attempt.success_count success,attempt.skipped_count skipped,
                    attempt.failed_count failed,attempt.error_code
               FROM workflow_attempts attempt
               LEFT JOIN scan_runs scan ON scan.id=attempt.root_scan_id)
             SELECT id,kind,mode,logical_run_id,attempt_no,predecessor_attempt_id,
                    actor,status,started_at,finished_at,parent_id,root_scan_id,
                    success,skipped,failed,error_code
             FROM history
             WHERE (?1 IS NULL OR kind=?1) AND (?2 IS NULL OR status=?2)
               AND (?3 IS NULL OR lower(id) LIKE '%' || lower(?3) || '%'
                    OR lower(logical_run_id) LIKE '%' || lower(?3) || '%')
               AND {comparison}
             ORDER BY started_at {ordering},id {ordering} LIMIT ?6"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![kind, status, query, cursor_time, cursor_key, limit],
                |r| {
                    Ok(HistoryRow {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        mode: r.get(2)?,
                        logical_run_id: r.get(3)?,
                        attempt_no: r.get::<_, i64>(4)? as u64,
                        predecessor_attempt_id: r.get(5)?,
                        actor: r.get(6)?,
                        status: r.get(7)?,
                        started_at: r.get(8)?,
                        finished_at: r.get(9)?,
                        parent_id: r.get(10)?,
                        root_scan_id: r.get(11)?,
                        success: r.get::<_, i64>(12)? as u64,
                        skipped: r.get::<_, i64>(13)? as u64,
                        failed: r.get::<_, i64>(14)? as u64,
                        error_code: r.get(15)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
    pub fn get_run_detail(&self, kind: &str, id: &str) -> Result<RunDetailRow, String> {
        if !matches!(
            kind,
            "scan" | "plan" | "apply" | "verify" | "rollback" | "recovery" | "archive"
        ) {
            return Err("invalid_run_kind".to_string());
        }
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        conn.query_row(
            "SELECT attempt.id,
                    CASE attempt.kind
                      WHEN 'plan_revision' THEN 'plan'
                      WHEN 'dry_run' THEN 'apply'
                      WHEN 'rollback_dry_run' THEN 'rollback'
                      WHEN 'recovery_dry_run' THEN 'recovery'
                      ELSE attempt.kind END display_kind,
                    attempt.logical_run_id,attempt.attempt_no,
                    attempt.predecessor_attempt_id,attempt.actor,
                    CASE WHEN scan.archived_at IS NOT NULL AND attempt.kind<>'archive'
                         THEN 'archived' ELSE attempt.status END,
                    attempt.parent_attempt_id,attempt.success_count,
                    attempt.skipped_count,attempt.failed_count,attempt.error_code
               FROM workflow_attempts attempt
               LEFT JOIN scan_runs scan ON scan.id=attempt.root_scan_id
              WHERE attempt.id=?1 AND
                    (CASE attempt.kind
                       WHEN 'plan_revision' THEN 'plan'
                       WHEN 'dry_run' THEN 'apply'
                       WHEN 'rollback_dry_run' THEN 'rollback'
                       WHEN 'recovery_dry_run' THEN 'recovery'
                       ELSE attempt.kind END)=?2",
            params![id, kind],
            |row| {
                Ok(RunDetailRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    logical_run_id: row.get(2)?,
                    attempt_no: row.get::<_, i64>(3)? as u64,
                    predecessor_attempt_id: row.get(4)?,
                    actor: row.get(5)?,
                    status: row.get(6)?,
                    parent_id: row.get(7)?,
                    success: row.get::<_, i64>(8)? as u64,
                    skipped: row.get::<_, i64>(9)? as u64,
                    failed: row.get::<_, i64>(10)? as u64,
                    error_code: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "run_not_found".to_string())
    }

    /// Bounded, attempt-scoped evidence for the history detail pane. This is
    /// deliberately a summary: the full path-bearing operation log remains a
    /// separately paged endpoint and is never transferred wholesale merely by
    /// selecting a history row.
    pub fn list_attempt_evidence(&self, attempt_id: &str) -> Result<AttemptEvidence, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut preflight_statement = conn
            .prepare(
                "SELECT id,kind,mode,status,passed_count,blocked_count
                   FROM preflight_runs
                  WHERE parent_attempt_id=?1
                  ORDER BY started_at,id LIMIT 32",
            )
            .map_err(|error| error.to_string())?;
        let preflights = preflight_statement
            .query_map(params![attempt_id], |row| {
                Ok(AttemptPreflightRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    mode: row.get(2)?,
                    status: row.get(3)?,
                    passed: row.get::<_, i64>(4)? as u64,
                    blocked: row.get::<_, i64>(5)? as u64,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;

        let mut journal_statement = conn
            .prepare(
                "SELECT id,sequence_no,direction,state,error
                   FROM operation_journal
                  WHERE attempt_id=?1
                     OR id IN (SELECT operation_id FROM recovery_runs WHERE id=?1)
                  ORDER BY sequence_no,id LIMIT 200",
            )
            .map_err(|error| error.to_string())?;
        let journal = journal_statement
            .query_map(params![attempt_id], |row| {
                Ok(AttemptJournalRow {
                    id: row.get(0)?,
                    sequence_no: row.get::<_, i64>(1)? as u64,
                    direction: row.get(2)?,
                    state: row.get(3)?,
                    error: row.get(4)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;

        let mut log_statement = conn
            .prepare(
                "SELECT sequence_no,phase,result,error FROM (
                     SELECT sequence_no,'apply' AS phase,result,error
                       FROM operation_logs WHERE execution_id=?1
                     UNION ALL
                     SELECT 0,'verify' AS phase,result,error
                       FROM verify_logs WHERE verify_run_id=?1
                     UNION ALL
                     SELECT 0,'rollback' AS phase,result,error
                       FROM rollback_logs WHERE rollback_run_id=?1
                 ) ORDER BY sequence_no,phase LIMIT 200",
            )
            .map_err(|error| error.to_string())?;
        let logs = log_statement
            .query_map(params![attempt_id], |row| {
                Ok(AttemptLogRow {
                    sequence_no: row.get::<_, i64>(0)? as u64,
                    phase: row.get(1)?,
                    result: row.get(2)?,
                    error: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(AttemptEvidence {
            preflights,
            journal,
            logs,
        })
    }
    /// Keyset pagination: callers retain the last ordinal; no OFFSET scan or full result transfer.
    pub fn list_plan_items(
        &self,
        plan_id: &str,
        after_ordinal: Option<u64>,
        limit: u32,
        query: Option<&str>,
        risk: Option<&str>,
    ) -> Result<PlanItemPage, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let after = after_ordinal.unwrap_or(0) as i64;
        let needle = query.unwrap_or("");
        let wanted_risk = risk.unwrap_or("");
        let page_size = limit.clamp(1, 500) as usize;
        let total = conn
            .query_row(
                "SELECT COUNT(*) FROM plan_items WHERE plan_id=?1",
                params![plan_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())? as u64;
        let filtered_total = conn
            .query_row(
                "SELECT COUNT(*) FROM plan_items WHERE plan_id=?1 AND (?2='' OR source_path LIKE '%' || ?2 || '%' OR target_path LIKE '%' || ?2 || '%') AND (?3='' OR risk=?3)",
                params![plan_id, needle, wanted_risk],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())? as u64;
        let counts = conn
            .query_row(
                "SELECT
                    SUM(CASE WHEN action='move' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN action='skip' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN risk<>'none' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN risk='conflict' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN risk='invalid_target' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN risk='metadata_missing' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN risk='path_too_long' THEN 1 ELSE 0 END)
                 FROM plan_items
                 WHERE plan_id=?1 AND (?2='' OR source_path LIKE '%' || ?2 || '%' OR target_path LIKE '%' || ?2 || '%')",
                params![plan_id, needle],
                |row| {
                    Ok(PlanItemCounts {
                        moves: row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64,
                        skips: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        needs_attention: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                        conflicts: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                        invalid_target: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                        metadata_missing: row.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                        path_too_long: row.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT i.id,i.conflict_group_id,
                        COALESCE(NULLIF((SELECT COUNT(*) FROM plan_conflict_candidates c WHERE c.conflict_group_id=i.conflict_group_id),0),
                                 (SELECT COUNT(*) FROM plan_conflict_members m WHERE m.conflict_group_id=i.conflict_group_id)),
                        i.ordinal,i.source_path,i.target_path,
                        i.source_path_encoding,i.source_path_blob,
                        i.target_path_encoding,i.target_path_blob,
                        i.action,i.risk,i.reason
                   FROM plan_items i
                  WHERE i.plan_id=?1 AND i.ordinal>?2
                    AND (?3='' OR i.source_path LIKE '%' || ?3 || '%' OR i.target_path LIKE '%' || ?3 || '%')
                    AND (?4='' OR i.risk=?4)
                  ORDER BY i.ordinal ASC LIMIT ?5",
            )
            .map_err(|error| error.to_string())?;
        let mut rows = stmt
            .query_map(
                params![plan_id, after, needle, wanted_risk, (page_size + 1) as i64],
                |r| {
                    let source = decode_row_path(r.get(6)?, r.get(7)?, 7)?;
                    let target = decode_optional_row_path(r.get(8)?, r.get(9)?, 9)?;
                    Ok(PlanItemRow {
                        id: r.get(0)?,
                        conflict_group_id: r.get(1)?,
                        conflict_member_count: r.get::<_, i64>(2)? as u64,
                        ordinal: r.get::<_, i64>(3)? as u64,
                        source_path: r.get(4)?,
                        source: path_envelope(&source, "plan_source"),
                        target_path: r.get(5)?,
                        target: target
                            .as_deref()
                            .map(|path| path_envelope(path, "plan_target")),
                        action: r.get(10)?,
                        risk: r.get(11)?,
                        reason: r.get(12)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let has_more = rows.len() > page_size;
        rows.truncate(page_size);
        let next_cursor = has_more.then(|| rows.last().expect("non-empty full page").ordinal);
        Ok(PlanItemPage {
            items: rows,
            total,
            filtered_total,
            next_cursor,
            counts,
        })
    }
    pub fn get_plan_conflict_detail(
        &self,
        plan_id: &str,
        conflict_group_id: &str,
    ) -> Result<PlanConflictDetail, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let (id, kind, target_path, existing_target_path, target_encoding, target_blob) = conn
            .query_row(
                "SELECT group_row.id,group_row.kind,group_row.target_path,
                        group_row.existing_target_path,
                        (SELECT item.target_path_encoding
                           FROM plan_conflict_members member
                           JOIN plan_items item ON item.id=member.plan_item_id
                          WHERE member.conflict_group_id=group_row.id
                            AND item.target_path_encoding IS NOT NULL
                          ORDER BY item.ordinal,item.id LIMIT 1),
                        (SELECT item.target_path_blob
                           FROM plan_conflict_members member
                           JOIN plan_items item ON item.id=member.plan_item_id
                          WHERE member.conflict_group_id=group_row.id
                            AND item.target_path_blob IS NOT NULL
                          ORDER BY item.ordinal,item.id LIMIT 1)
                   FROM plan_conflict_groups group_row
                  WHERE group_row.id=?1 AND group_row.plan_id=?2",
                params![conflict_group_id, plan_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan_conflict_not_found".to_string())?;
        let target_native = match (target_encoding, target_blob) {
            (Some(encoding), Some(blob)) => Some(decode_path(&encoding, &blob)?),
            (None, None) => None,
            _ => return Err("plan_conflict_target_codec_incomplete".into()),
        };
        let mut statement = conn
            .prepare("SELECT i.id,i.ordinal,i.source_path,i.source_path_encoding,i.source_path_blob FROM plan_conflict_members m JOIN plan_items i ON i.id=m.plan_item_id WHERE m.conflict_group_id=?1 ORDER BY i.ordinal,i.id")
            .map_err(|error| error.to_string())?;
        let members = statement
            .query_map(params![conflict_group_id], |row| {
                let source = decode_row_path(row.get(3)?, row.get(4)?, 4)?;
                Ok(PlanConflictMemberRow {
                    item_id: row.get(0)?,
                    ordinal: row.get::<_, i64>(1)? as u64,
                    source_path: row.get(2)?,
                    source: path_envelope(&source, "conflict_source"),
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let mut candidate_statement = conn
            .prepare("SELECT ordinal,target_path_encoding,target_path_blob FROM plan_conflict_candidates WHERE conflict_group_id=?1 ORDER BY ordinal")
            .map_err(|error| error.to_string())?;
        let candidate_rows = candidate_statement
            .query_map(params![conflict_group_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let mut candidates = Vec::with_capacity(candidate_rows.len());
        for (ordinal, target_encoding, target_blob) in candidate_rows {
            let mut member_statement = conn
                .prepare("SELECT i.id,i.ordinal,i.source_path,i.source_path_encoding,i.source_path_blob FROM plan_conflict_candidate_members m JOIN plan_items i ON i.id=m.plan_item_id WHERE m.conflict_group_id=?1 AND m.candidate_ordinal=?2 ORDER BY i.ordinal,i.id")
                .map_err(|error| error.to_string())?;
            let candidate_members = member_statement
                .query_map(params![conflict_group_id, ordinal], |row| {
                    let source = decode_row_path(row.get(3)?, row.get(4)?, 4)?;
                    Ok(PlanConflictMemberRow {
                        item_id: row.get(0)?,
                        ordinal: row.get::<_, i64>(1)? as u64,
                        source_path: row.get(2)?,
                        source: path_envelope(&source, "conflict_candidate_source"),
                    })
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let candidate_target = decode_path(&target_encoding, &target_blob)?;
            candidates.push(PlanConflictCandidateRow {
                ordinal: u64::try_from(ordinal)
                    .map_err(|_| "plan_conflict_candidate_ordinal_invalid")?,
                target_path: candidate_target.to_string_lossy().into_owned(),
                target: path_envelope(&candidate_target, "conflict_candidate_target"),
                members: candidate_members,
            });
        }
        let target_envelope = target_native
            .as_deref()
            .map(|path| path_envelope(path, "conflict_target"));
        let existing_target_envelope = target_native
            .as_deref()
            .filter(|_| existing_target_path.is_some())
            .map(|path| path_envelope(path, "existing_conflict_target"));
        Ok(PlanConflictDetail {
            id,
            kind,
            target_path,
            target: target_envelope,
            existing_target_path,
            existing_target: existing_target_envelope,
            members,
            candidates,
        })
    }

    /// Resolves an opaque conflict-candidate ordinal to the final image target.
    ///
    /// The display path is deliberately not accepted as input. Every identifier
    /// is bound back to the immutable completed Plan, both persisted paths are
    /// decoded losslessly, and Core revalidates the composed target capability.
    pub fn resolve_plan_conflict_candidate_target(
        &self,
        plan_id: &str,
        plan_item_id: &str,
        conflict_group_id: &str,
        candidate_ordinal: u64,
    ) -> Result<PathBuf, String> {
        let candidate_ordinal = i64::try_from(candidate_ordinal)
            .map_err(|_| "plan_conflict_candidate_ordinal_invalid".to_string())?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        type CandidateTargetContext = (
            String,
            Option<i64>,
            String,
            Vec<u8>,
            String,
            i64,
            i64,
            i64,
            String,
            Vec<u8>,
            String,
            Vec<u8>,
        );
        let persisted: Option<CandidateTargetContext> = conn
            .query_row(
                "SELECT plan.status,plan.archived_at,
                        plan.target_root_encoding,plan.target_root_blob,
                        plan.rules_json,plan.rules_schema_version,
                        plan.snapshot_version,plan.path_policy_version,
                        item.source_path_encoding,item.source_path_blob,
                        candidate.target_path_encoding,candidate.target_path_blob
                   FROM plan_runs plan
                   JOIN plan_items item
                     ON item.plan_id=plan.id AND item.id=?2
                   JOIN plan_conflict_groups conflict
                     ON conflict.plan_id=plan.id AND conflict.id=?3
                        AND conflict.kind='image_destination'
                   JOIN plan_conflict_members member
                     ON member.conflict_group_id=conflict.id
                        AND member.plan_item_id=item.id
                   JOIN plan_conflict_candidates candidate
                     ON candidate.conflict_group_id=conflict.id
                        AND candidate.ordinal=?4
                  WHERE plan.id=?1 AND item.conflict_group_id=conflict.id",
                params![plan_id, plan_item_id, conflict_group_id, candidate_ordinal],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let (
            status,
            archived_at,
            target_root_encoding,
            target_root_blob,
            rules_json,
            rules_schema_version,
            snapshot_version,
            path_policy_version,
            source_encoding,
            source_blob,
            candidate_encoding,
            candidate_blob,
        ) = persisted.ok_or_else(|| "plan_conflict_candidate_not_found".to_string())?;
        if status != "completed" || archived_at.is_some() {
            return Err("plan_conflict_candidate_not_authorizable".into());
        }
        if snapshot_version != i64::from(PLAN_SNAPSHOT_VERSION)
            || path_policy_version != i64::from(music_folder_core::WINDOWS_PATH_POLICY_VERSION)
        {
            return Err("legacy_plan_non_revisable".into());
        }
        let naming = current_naming_rules(
            rules_schema_version,
            &rules_json,
            "legacy_plan_non_revisable",
        )?;
        let target_root = decode_path(&target_root_encoding, &target_root_blob)?;
        let source_path = decode_path(&source_encoding, &source_blob)?;
        let file_name = source_path
            .file_name()
            .ok_or_else(|| "plan_conflict_candidate_source_filename_missing".to_string())?;
        let candidate_directory = decode_path(&candidate_encoding, &candidate_blob)?;
        let target = candidate_directory.join(file_name);
        SafeTargetPath::from_completed_target(&target_root, &target, naming.allow_long_paths)
            .map(SafeTargetPath::into_path_buf)
            .map_err(|error| {
                format!(
                    "plan_conflict_candidate_target_unsafe:{}",
                    error.reason_code()
                )
            })
    }

    pub fn list_operation_logs(
        &self,
        execution_id: &str,
        after_sequence: Option<u64>,
        limit: u32,
        query: Option<&str>,
        result: Option<&str>,
    ) -> Result<Vec<OperationLogRow>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let after = after_sequence.unwrap_or(0) as i64;
        let q = query.unwrap_or("");
        let status = result.unwrap_or("");
        let mut stmt=conn.prepare("SELECT id,execution_id,sequence_no,source_path,target_path,source_path_encoding,source_path_blob,target_path_encoding,target_path_blob,action,result,error,created_at FROM operation_logs WHERE execution_id=?1 AND sequence_no>?2 AND (?3='' OR source_path LIKE '%'||?3||'%' OR target_path LIKE '%'||?3||'%' OR error LIKE '%'||?3||'%') AND (?4='' OR result=?4) ORDER BY sequence_no,id LIMIT ?5").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(
                params![execution_id, after, q, status, limit.min(500) as i64],
                |r| {
                    let source = decode_row_path(r.get(5)?, r.get(6)?, 6)?;
                    let target = decode_optional_row_path(r.get(7)?, r.get(8)?, 8)?;
                    Ok(OperationLogRow {
                        id: r.get(0)?,
                        execution_id: r.get(1)?,
                        sequence_no: r.get::<_, i64>(2)? as u64,
                        source_path: r.get(3)?,
                        source: path_envelope(&source, "operation_source"),
                        target_path: r.get(4)?,
                        target: target
                            .as_deref()
                            .map(|path| path_envelope(path, "operation_target")),
                        action: r.get(9)?,
                        result: r.get(10)?,
                        error: r.get(11)?,
                        created_at: r.get(12)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
    pub fn list_metrics(&self, run_id: &str) -> Result<Vec<MetricRow>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut stmt=conn.prepare("SELECT phase,elapsed_ms,item_count FROM run_metrics WHERE run_id=?1 ORDER BY rowid").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok(MetricRow {
                    phase: r.get(0)?,
                    elapsed_ms: r.get::<_, i64>(1)? as u64,
                    item_count: r.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
    pub fn delete_history(&self, kind: &str, id: &str) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        if kind != "scan" {
            return Err("history_purge_requires_root_scan".into());
        }
        // Fence mutation/attempt writers before evaluating any purge
        // precondition. A deferred transaction would leave a check-to-delete
        // window in which another process could start an attempt.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        tx.execute_batch("PRAGMA defer_foreign_keys=ON;")
            .map_err(|error| error.to_string())?;
        let root_scan_id = archive::root_scan_id_for(&tx, kind, id)?;
        archive::verify_completed_archive(&tx, &root_scan_id)?;
        let protection_reasons = archive::archive_block_reasons(&tx, &root_scan_id)?;
        if !protection_reasons.is_empty() {
            return Err(format!(
                "history_purge_blocked:{}",
                protection_reasons.join(",")
            ));
        }
        let running: Option<String> = match kind {
            "scan" => tx
                .query_row(
                    "SELECT id FROM scan_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "plan" => tx
                .query_row(
                    "SELECT id FROM plan_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "apply" => tx
                .query_row(
                    "SELECT id FROM execution_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "verify" => tx
                .query_row(
                    "SELECT id FROM verify_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "rollback" => tx
                .query_row(
                    "SELECT id FROM rollback_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            _ => return Err("invalid_run_kind".into()),
        }
        .map_err(|e| e.to_string())?;
        if running.is_some() {
            return Err("running_run_cannot_be_deleted".into());
        }
        // A parent deletion must include all dependent plans/executions.  Refuse it
        // whenever any descendant is active; completed history is then deleted in
        // child-to-parent order below.
        if matches!(kind, "scan" | "plan") {
            let seed = if kind == "scan" { "scan_id" } else { "id" };
            let sql = format!("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE {seed}=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) SELECT 1 FROM plan_runs WHERE id IN (SELECT id FROM plans) AND status='running' UNION ALL SELECT 1 FROM execution_runs WHERE plan_id IN (SELECT id FROM plans) AND status='running' UNION ALL SELECT 1 FROM verify_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)) AND status='running' UNION ALL SELECT 1 FROM rollback_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)) AND status='running' LIMIT 1");
            let active: Option<i64> = tx
                .query_row(&sql, params![id], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if active.is_some() {
                return Err("dependent_running_run_cannot_be_deleted".into());
            }
        }
        tx.execute(
            "WITH plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1),
                  attempts(id) AS (
                    SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)
                    UNION SELECT id FROM rollback_runs WHERE execution_id IN (
                        SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)
                    )
                  )
             DELETE FROM recovery_runs WHERE operation_id IN (
                 SELECT id FROM operation_journal WHERE attempt_id IN (SELECT id FROM attempts)
             )",
            params![root_scan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "WITH plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1),
                  attempts(id) AS (
                    SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)
                    UNION SELECT id FROM rollback_runs WHERE execution_id IN (
                        SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)
                    )
                  )
             DELETE FROM operation_journal WHERE attempt_id IN (SELECT id FROM attempts)",
            params![root_scan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_builds WHERE plan_id IN (SELECT id FROM plan_runs WHERE scan_id=?1)",
            params![root_scan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM run_metrics WHERE run_id=?1 OR run_id IN (SELECT id FROM plan_runs WHERE scan_id=?1) OR run_id IN (SELECT id FROM workflow_attempts WHERE root_scan_id=?1)",
            params![root_scan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM diagnostic_events WHERE run_id=?1 OR run_id IN (SELECT id FROM plan_runs WHERE scan_id=?1) OR attempt_id IN (SELECT id FROM workflow_attempts WHERE root_scan_id=?1)",
            params![root_scan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM preflight_runs
              WHERE parent_attempt_id IN (
                    SELECT id FROM workflow_attempts WHERE root_scan_id=?1
              )",
            params![root_scan_id],
        )
        .map_err(|error| error.to_string())?;
        match kind {
            "scan" => {
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM verify_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM rollback_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM verify_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM rollback_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM operation_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute(
                    "DELETE FROM workflow_attempts WHERE root_scan_id=?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_revision_changes WHERE child_plan_id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_runs WHERE id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute(
                    "DELETE FROM plan_items WHERE plan_id NOT IN (SELECT id FROM plan_runs)",
                    [],
                )
                .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM scan_items WHERE scan_id=?1", params![id])
                    .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM scan_warnings WHERE scan_id=?1", params![id])
                    .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM scan_runs WHERE id=?1", params![id])
                    .map_err(|e| e.to_string())?;
            }
            "plan" => {
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM verify_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM rollback_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM verify_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM rollback_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM operation_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_runs WHERE id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute(
                    "DELETE FROM plan_items WHERE plan_id NOT IN (SELECT id FROM plan_runs)",
                    [],
                )
                .map_err(|e| e.to_string())?;
            }
            "apply" => {
                tx.execute("DELETE FROM verify_logs WHERE execution_id=?1", params![id])
                    .map_err(|e| e.to_string())?;
                tx.execute(
                    "DELETE FROM rollback_logs WHERE execution_id=?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM verify_runs WHERE execution_id=?1", params![id])
                    .map_err(|e| e.to_string())?;
                tx.execute(
                    "DELETE FROM rollback_runs WHERE execution_id=?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
                tx.execute(
                    "DELETE FROM operation_logs WHERE execution_id=?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM execution_runs WHERE id=?1", params![id])
                    .map_err(|e| e.to_string())?;
            }
            "verify" => {
                tx.execute("DELETE FROM verify_logs WHERE execution_id=(SELECT execution_id FROM verify_runs WHERE id=?1)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("DELETE FROM verify_runs WHERE id=?1", params![id])
                    .map_err(|e| e.to_string())?;
            }
            "rollback" => {
                tx.execute("DELETE FROM rollback_logs WHERE execution_id=(SELECT execution_id FROM rollback_runs WHERE id=?1)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("DELETE FROM rollback_runs WHERE id=?1", params![id])
                    .map_err(|e| e.to_string())?;
            }
            _ => unreachable!(),
        }
        tx.execute(
            "INSERT INTO diagnostic_events(id,run_id,class,code,payload_json,contains_sensitive_path,protected,created_at,severity,phase,message_key) VALUES(?1,?2,'audit','history_purged',?3,0,1,?4,'info','retention','history_purged')",
            params![
                Uuid::new_v4().to_string(),
                root_scan_id,
                serde_json::json!({"archive_verified": true}).to_string(),
                now()
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn history_cleanup_preview(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<HistoryCleanupPreview, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let root_scan_id = archive::root_scan_id_for(&conn, kind, id)?;
        let (plans, executions, logs): (i64, i64, i64) = conn
            .query_row(
                "WITH plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1),
                      executions(id) AS (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)),
                      attempts(id) AS (SELECT id FROM workflow_attempts WHERE root_scan_id=?1),
                      journals(id) AS (SELECT id FROM operation_journal WHERE attempt_id IN (SELECT id FROM attempts))
                 SELECT (SELECT COUNT(*) FROM plans),
                        (SELECT COUNT(*) FROM executions),
                        (SELECT COUNT(*) FROM operation_logs WHERE execution_id IN (SELECT id FROM executions))
                        + (SELECT COUNT(*) FROM verify_logs WHERE execution_id IN (SELECT id FROM executions))
                        + (SELECT COUNT(*) FROM rollback_logs WHERE execution_id IN (SELECT id FROM executions))
                        + (SELECT COUNT(*) FROM attempts)
                        + (SELECT COUNT(*) FROM journals)
                        + (SELECT COUNT(*) FROM operation_journal_events WHERE operation_id IN (SELECT id FROM journals))
                        + (SELECT COUNT(*) FROM plan_builds WHERE plan_id IN (SELECT id FROM plans))
                        + (SELECT COUNT(*) FROM plan_revision_changes WHERE child_plan_id IN (SELECT id FROM plans))",
                params![root_scan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| error.to_string())?;
        let protection_reasons = archive::archive_block_reasons(&conn, &root_scan_id)?;
        let archived: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM archive_manifests WHERE root_scan_id=?1 AND status='completed')",
                params![root_scan_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            != 0;
        let record_estimate = 1_u64
            .saturating_add(plans.max(0) as u64)
            .saturating_add(executions.max(0) as u64)
            .saturating_add(logs.max(0) as u64);
        let archive_path = self
            .database_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("history-archives");
        Ok(HistoryCleanupPreview {
            root_scan_id,
            plans: plans.max(0) as u64,
            executions: executions.max(0) as u64,
            logs: logs.max(0) as u64,
            blocked: !protection_reasons.is_empty(),
            protection_reasons,
            estimated_bytes: record_estimate.saturating_mul(512),
            archive_path,
            archived,
        })
    }
    fn record_metric_row(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .execute(
                "INSERT INTO run_metrics(run_id,phase,elapsed_ms,item_count) VALUES(?1,?2,?3,?4)",
                params![run_id, phase, elapsed_ms as i64, item_count as i64],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn decode_row_path(encoding: String, raw: Vec<u8>, column: usize) -> rusqlite::Result<PathBuf> {
    decode_path(&encoding, &raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Blob,
            std::io::Error::other(error).into(),
        )
    })
}

fn decode_optional_row_path(
    encoding: Option<String>,
    raw: Option<Vec<u8>>,
    column: usize,
) -> rusqlite::Result<Option<PathBuf>> {
    match (encoding, raw) {
        (Some(encoding), Some(raw)) => decode_row_path(encoding, raw, column).map(Some),
        (None, None) => Ok(None),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn verify_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VerifyItem> {
    let action = row.get::<_, String>(6)?;
    let source_encoding = row.get::<_, String>(2)?;
    let source_blob = row.get::<_, Vec<u8>>(3)?;
    let target_encoding = row.get::<_, Option<String>>(4)?;
    let target_blob = row.get::<_, Option<Vec<u8>>>(5)?;
    let source = decode_path(&source_encoding, &source_blob).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Blob,
            std::io::Error::other(error).into(),
        )
    })?;
    let target = match (target_encoding, target_blob) {
        (Some(encoding), Some(blob)) => Some(decode_path(&encoding, &blob).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Blob,
                std::io::Error::other(error).into(),
            )
        })?),
        (None, None) => None,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(VerifyItem {
        operation_id: row.get(0)?,
        sequence_no: u64::try_from(row.get::<_, i64>(1)?).map_err(|_| {
            rusqlite::Error::IntegralValueOutOfRange(1, row.get::<_, i64>(1).unwrap_or(-1))
        })?,
        source,
        target,
        action: OperationAction::from_code(&action).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(6, "action".into(), rusqlite::types::Type::Text)
        })?,
        expected_size: row
            .get::<_, Option<i64>>(7)?
            .map(|value| {
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, value))
            })
            .transpose()?,
        expected_content_sha256: row.get(8)?,
        expected_file_identity: row.get(9)?,
    })
}

fn journal_state_from_code(value: &str) -> Result<JournalState, String> {
    match value {
        "prepared" => Ok(JournalState::Prepared),
        "staging" => Ok(JournalState::Staging),
        "staged" => Ok(JournalState::Staged),
        "content_verified" => Ok(JournalState::ContentVerified),
        "published" => Ok(JournalState::Published),
        "source_deleted" => Ok(JournalState::SourceDeleted),
        "completed" => Ok(JournalState::Completed),
        "failed" => Ok(JournalState::Failed),
        "recovery_required" => Ok(JournalState::RecoveryRequired),
        unknown => Err(format!("journal_state_unknown:{unknown}")),
    }
}

/// Rebuilds the denormalized operation/rollback summaries when a process died
/// after the authoritative journal reached a terminal state. The deterministic
/// ids and sequence guards make the repair idempotent across repeated opens.
fn reconcile_terminal_journal_summaries(
    transaction: &Transaction<'_>,
    current_time: i64,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO operation_logs(
                 id,execution_id,plan_item_id,sequence_no,
                 source_path,source_path_encoding,source_path_blob,
                 target_path,target_path_encoding,target_path_blob,
                 action,result,error,source_deleted,expected_size,
                 expected_content_sha256,expected_file_identity,created_at
             )
             SELECT 'journal-summary-' || journal.id,
                    journal.attempt_id,journal.plan_item_id,journal.sequence_no,
                    journal.source_path,journal.source_path_encoding,journal.source_path_blob,
                    journal.target_path,journal.target_path_encoding,journal.target_path_blob,
                    CASE journal.strategy
                      WHEN 'atomic_no_replace_rename' THEN 'move'
                      ELSE 'copy_delete'
                    END,
                    CASE journal.state WHEN 'completed' THEN 'success' ELSE 'failed' END,
                    CASE journal.state WHEN 'completed' THEN NULL
                      ELSE COALESCE(journal.error,'journal_terminal_without_summary') END,
                    CASE journal.state WHEN 'completed' THEN 1 ELSE 0 END,
                    journal.expected_size,journal.expected_content_sha256,
                    CASE journal.strategy WHEN 'copy_publish_delete'
                         THEN COALESCE(journal.staged_file_identity,journal.expected_file_identity)
                         ELSE journal.expected_file_identity END,?1
               FROM operation_journal journal
               JOIN execution_runs execution ON execution.id=journal.attempt_id
              WHERE journal.direction='apply'
                AND journal.state IN ('completed','failed','recovery_required')
                AND execution.mode='apply'
                AND NOT EXISTS(
                    SELECT 1 FROM operation_logs summary
                     WHERE summary.execution_id=journal.attempt_id
                       AND summary.sequence_no=journal.sequence_no
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO rollback_logs(
                 id,execution_id,rollback_run_id,operation_id,result,error,created_at
             )
             SELECT 'journal-summary-' || journal.id,
                    rollback.execution_id,journal.attempt_id,journal.plan_item_id,
                    CASE journal.state WHEN 'completed' THEN 'success' ELSE 'failed' END,
                    CASE journal.state WHEN 'completed' THEN NULL
                      ELSE COALESCE(journal.error,'journal_terminal_without_summary') END,
                    ?1
               FROM operation_journal journal
               JOIN rollback_runs rollback ON rollback.id=journal.attempt_id
               JOIN operation_logs operation
                 ON operation.id=journal.plan_item_id
                AND operation.execution_id=rollback.execution_id
              WHERE journal.direction='rollback'
                AND journal.state IN ('completed','failed','recovery_required')
                AND rollback.mode='rollback'
                AND NOT EXISTS(
                    SELECT 1 FROM rollback_logs summary
                     WHERE summary.rollback_run_id=journal.attempt_id
                       AND summary.operation_id=journal.plan_item_id
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    // Diagnostics must never make the repair fail. The payload intentionally
    // contains only stable codes and no native/display paths.
    let _ = transaction.execute(
        "INSERT OR IGNORE INTO diagnostic_events(
             id,run_id,attempt_id,class,code,payload_json,
             contains_sensitive_path,protected,created_at,severity,phase,item_id,
             sequence_no,path_role,payload_schema_version,message_key
         )
         SELECT 'journal-summary-diagnostic-' || journal.id,
                attempt.logical_run_id,journal.attempt_id,
                CASE journal.state WHEN 'recovery_required' THEN 'recovery'
                     ELSE 'audit' END,
                'journal_summary_reconciled',
                '{\"cause_chain\":[{\"code\":\"journal_terminal_without_summary\"}]}',
                0,CASE journal.state WHEN 'recovery_required' THEN 1 ELSE 0 END,
                ?1,'warning',journal.direction,journal.plan_item_id,
                journal.sequence_no,NULL,1,
                CASE journal.state WHEN 'recovery_required'
                     THEN 'workflow_recovery_required'
                     ELSE 'journal_summary_reconciled' END
           FROM operation_journal journal
           JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id
          WHERE journal.state IN ('completed','failed','recovery_required')
            AND (
                (journal.direction='apply' AND EXISTS(
                    SELECT 1 FROM operation_logs summary
                     WHERE summary.id='journal-summary-' || journal.id
                ))
             OR (journal.direction='rollback' AND EXISTS(
                    SELECT 1 FROM rollback_logs summary
                     WHERE summary.id='journal-summary-' || journal.id
                ))
            )",
        params![current_time],
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct TerminalJournalOutcome {
    status: RunStatus,
    success: u64,
    skipped: u64,
    failed: u64,
}

fn terminal_apply_journal_outcome(
    transaction: &Transaction<'_>,
    execution_id: &str,
) -> Result<Option<TerminalJournalOutcome>, String> {
    let (journal_count, nonterminal_count): (i64, i64) = transaction
        .query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN state NOT IN ('completed','failed') THEN 1 ELSE 0 END)
               FROM operation_journal
              WHERE attempt_id=?1 AND direction='apply'",
            params![execution_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .map_err(|error| error.to_string())?;
    if journal_count == 0 || nonterminal_count != 0 {
        return Ok(None);
    }
    let (success, skipped, failed, observed, expected): (i64, i64, i64, i64, i64) = transaction
        .query_row(
            "WITH authoritative(sequence_no,result) AS (
                 SELECT journal.sequence_no,
                        CASE journal.state WHEN 'completed' THEN 'success' ELSE 'failed' END
                   FROM operation_journal journal
                  WHERE journal.attempt_id=?1 AND journal.direction='apply'
                 UNION ALL
                 SELECT summary.sequence_no,summary.result
                   FROM operation_logs summary
                  WHERE summary.execution_id=?1
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=summary.execution_id
                           AND journal.direction='apply'
                           AND journal.sequence_no=summary.sequence_no
                    )
             )
             SELECT COALESCE(SUM(result='success'),0),
                    COALESCE(SUM(result='skipped'),0),
                    COALESCE(SUM(result='failed'),0),
                    COUNT(*),
                    (SELECT COUNT(*) FROM plan_items item
                      WHERE item.plan_id=(SELECT plan_id FROM execution_runs WHERE id=?1))
               FROM authoritative",
            params![execution_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;
    let missing = expected.saturating_sub(observed);
    let failed = failed.saturating_add(missing);
    Ok(Some(TerminalJournalOutcome {
        status: if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Partial
        },
        success: success.max(0) as u64,
        skipped: skipped.max(0) as u64,
        failed: failed.max(0) as u64,
    }))
}

fn terminal_rollback_journal_outcome(
    transaction: &Transaction<'_>,
    rollback_id: &str,
) -> Result<Option<TerminalJournalOutcome>, String> {
    let (journal_count, nonterminal_count): (i64, i64) = transaction
        .query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN state NOT IN ('completed','failed') THEN 1 ELSE 0 END)
               FROM operation_journal
              WHERE attempt_id=?1 AND direction='rollback'",
            params![rollback_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .map_err(|error| error.to_string())?;
    if journal_count == 0 || nonterminal_count != 0 {
        return Ok(None);
    }
    let (success, skipped, failed, observed, expected): (i64, i64, i64, i64, i64) = transaction
        .query_row(
            "WITH rollback_execution(id) AS (
                 SELECT execution_id FROM rollback_runs WHERE id=?1
             ),
             expected_items(operation_id,sequence_no) AS (
                 SELECT operation.id,journal.sequence_no
                   FROM operation_journal journal
                   JOIN operation_logs operation
                     ON operation.execution_id=journal.attempt_id
                    AND operation.sequence_no=journal.sequence_no
                  WHERE journal.attempt_id=(SELECT id FROM rollback_execution)
                    AND journal.direction='apply' AND journal.state='completed'
                 UNION ALL
                 SELECT operation.id,operation.sequence_no
                   FROM operation_logs operation
                  WHERE operation.execution_id=(SELECT id FROM rollback_execution)
                    AND ((operation.result='success'
                          AND operation.action IN ('move','copy_delete'))
                      OR (operation.result='failed'
                          AND operation.action='copy_source_retained'))
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=operation.execution_id
                           AND journal.direction='apply'
                           AND journal.sequence_no=operation.sequence_no
                           AND journal.state='completed'
                    )
             ),
             authoritative(operation_id,result) AS (
                 SELECT journal.plan_item_id,
                        CASE journal.state WHEN 'completed' THEN 'success' ELSE 'failed' END
                   FROM operation_journal journal
                  WHERE journal.attempt_id=?1 AND journal.direction='rollback'
                 UNION ALL
                 SELECT summary.operation_id,summary.result
                   FROM rollback_logs summary
                  WHERE summary.rollback_run_id=?1
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=summary.rollback_run_id
                           AND journal.direction='rollback'
                           AND journal.plan_item_id=summary.operation_id
                    )
             )
             SELECT COALESCE(SUM(result='success'),0),
                    COALESCE(SUM(result='skipped'),0),
                    COALESCE(SUM(result='failed'),0),
                    COUNT(*),(SELECT COUNT(*) FROM expected_items)
               FROM authoritative",
            params![rollback_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;
    let missing = expected.saturating_sub(observed);
    let failed = failed.saturating_add(missing);
    Ok(Some(TerminalJournalOutcome {
        status: if failed == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Partial
        },
        success: success.max(0) as u64,
        skipped: skipped.max(0) as u64,
        failed: failed.max(0) as u64,
    }))
}

fn persist_terminal_apply_outcome(
    transaction: &Transaction<'_>,
    execution_id: &str,
    finished_at: i64,
) -> Result<Option<TerminalJournalOutcome>, String> {
    let Some(outcome) = terminal_apply_journal_outcome(transaction, execution_id)? else {
        return Ok(None);
    };
    let changed = transaction
        .execute(
            "UPDATE execution_runs
                SET status=?2,finished_at=COALESCE(finished_at,?3),
                    success_count=?4,skipped_count=?5,failed_count=?6
              WHERE id=?1 AND mode='apply' AND status IN ('running','recovery_required')",
            params![
                execution_id,
                outcome.status.as_str(),
                finished_at,
                outcome.success as i64,
                outcome.skipped as i64,
                outcome.failed as i64
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 0 {
        transaction
            .execute(
                "UPDATE workflow_attempts
                    SET status=?2,finished_at=COALESCE(finished_at,?3),
                        success_count=?4,skipped_count=?5,failed_count=?6,
                        error_code=CASE WHEN ?2='completed' THEN NULL
                                        ELSE COALESCE(error_code,'journal_summary_reconciled') END
                  WHERE id=?1 AND status IN ('running','recovery_required')",
                params![
                    execution_id,
                    outcome.status.as_str(),
                    finished_at,
                    outcome.success as i64,
                    outcome.skipped as i64,
                    outcome.failed as i64
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(Some(outcome))
}

fn persist_terminal_rollback_outcome(
    transaction: &Transaction<'_>,
    rollback_id: &str,
    finished_at: i64,
) -> Result<Option<TerminalJournalOutcome>, String> {
    let Some(outcome) = terminal_rollback_journal_outcome(transaction, rollback_id)? else {
        return Ok(None);
    };
    let changed = transaction
        .execute(
            "UPDATE rollback_runs
                SET status=?2,finished_at=COALESCE(finished_at,?3),
                    success_count=?4,skipped_count=?5,failed_count=?6
              WHERE id=?1 AND mode='rollback' AND status IN ('running','recovery_required')",
            params![
                rollback_id,
                outcome.status.as_str(),
                finished_at,
                outcome.success as i64,
                outcome.skipped as i64,
                outcome.failed as i64
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 0 {
        transaction
            .execute(
                "UPDATE workflow_attempts
                    SET status=?2,finished_at=COALESCE(finished_at,?3),
                        success_count=?4,skipped_count=?5,failed_count=?6,
                        error_code=CASE WHEN ?2='completed' THEN NULL
                                        ELSE COALESCE(error_code,'journal_summary_reconciled') END
                  WHERE id=?1 AND status IN ('running','recovery_required')",
                params![
                    rollback_id,
                    outcome.status.as_str(),
                    finished_at,
                    outcome.success as i64,
                    outcome.skipped as i64,
                    outcome.failed as i64
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(Some(outcome))
}

/// Converts an operation left behind by a dead/expired lease into an explicit
/// recovery state.  Merely reopening the database must never blindly resume a
/// filesystem mutation.
fn reconcile_abandoned_journals(connection: &mut Connection) -> Result<(), String> {
    let current_time = now();
    let persisted_leases = {
        let mut statement = connection
            .prepare(
                "SELECT fencing_token,scope_path_encoding,scope_path_blob
                   FROM mutation_leases",
            )
            .map_err(|error| error.to_string())?;
        let leases = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        leases
    };
    let mut leases_by_token = HashMap::<i64, (Vec<PathBuf>, bool)>::new();
    for (token, encoding, raw) in persisted_leases {
        let entry = leases_by_token
            .entry(token)
            .or_insert_with(|| (Vec::new(), false));
        match decode_path(&encoding, &raw) {
            Ok(scope) => entry.0.push(scope),
            Err(_) => entry.1 = true,
        }
    }
    let mut candidates = Vec::new();
    for (token, (scopes, decode_failed)) in leases_by_token {
        if !decode_failed && !scopes.is_empty() && RootProcessLock::acquire_many(&scopes).is_ok() {
            candidates.push((token, scopes));
        }
    }
    candidates.sort_by_key(|(token, _)| *token);
    let candidate_scopes = candidates
        .iter()
        .flat_map(|(_, scopes)| scopes.iter().cloned())
        .collect::<Vec<_>>();
    // Reacquire every candidate as one guard and retain it until commit. This
    // closes the probe/delete race with a concurrent acquire on another DB
    // connection or process.
    let (abandoned_tokens, _reconciliation_guard) = if candidate_scopes.is_empty() {
        (Vec::new(), None)
    } else {
        match RootProcessLock::acquire_many(&candidate_scopes) {
            Ok(guard) => (
                candidates
                    .into_iter()
                    .map(|(token, _)| token)
                    .collect::<Vec<_>>(),
                Some(guard),
            ),
            Err(_) => (Vec::new(), None),
        }
    };
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    for token in abandoned_tokens {
        transaction
            .execute(
                "DELETE FROM mutation_leases WHERE fencing_token=?1",
                params![token],
            )
            .map_err(|error| error.to_string())?;
    }
    let abandoned_operations = {
        let mut statement = transaction
            .prepare(
                "SELECT journal.id,journal.state,journal.fencing_token,journal.transition_no,
                        journal.attempt_id,
                        COALESCE(attempt.logical_run_id,journal.attempt_id),
                        journal.plan_item_id,journal.sequence_no,journal.direction
                  FROM operation_journal journal
                  LEFT JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id
                  WHERE journal.state NOT IN ('completed','failed')
                    AND NOT EXISTS (
                        SELECT 1 FROM mutation_leases lease
                         WHERE lease.fencing_token=journal.fencing_token
                    )
                  ORDER BY journal.attempt_id,journal.sequence_no,journal.id",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    for (
        operation_id,
        state,
        fencing_token,
        previous_transition_no,
        attempt_id,
        logical_run_id,
        plan_item_id,
        sequence_no,
        direction,
    ) in abandoned_operations
    {
        let changed = transaction
            .execute(
                "UPDATE operation_journal
                    SET error=CASE WHEN error IS NULL THEN 'mutation_lease_abandoned'
                                   WHEN instr(error,'mutation_lease_abandoned')=0
                                   THEN error || ';mutation_lease_abandoned'
                                   ELSE error END,
                        transition_no=transition_no+1,updated_at=?5
                  WHERE id=?1 AND state=?2 AND fencing_token=?3 AND transition_no=?4
                    AND NOT EXISTS (
                        SELECT 1 FROM mutation_leases lease
                         WHERE lease.fencing_token=operation_journal.fencing_token
                    )",
                params![
                    operation_id,
                    state,
                    fencing_token,
                    previous_transition_no,
                    current_time
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("journal_reconciliation_cas_rejected".into());
        }
        transaction
            .execute(
                "INSERT INTO operation_journal_events(
                     operation_id,transition_no,from_state,to_state,fencing_token,
                     payload_schema_version,payload_json,created_at
                 ) VALUES(?1,?2,?3,?3,?4,1,?5,?6)",
                params![
                    operation_id,
                    previous_transition_no + 1,
                    state,
                    fencing_token,
                    serde_json::json!({
                        "event": "lease_reconciliation",
                        "reason": "mutation_lease_abandoned"
                    })
                    .to_string(),
                    current_time
                ],
            )
            .map_err(|error| error.to_string())?;
        diagnostics::insert_diagnostic_best_effort(
            &transaction,
            DiagnosticEventInput {
                run_id: Some(logical_run_id),
                attempt_id: Some(attempt_id),
                class: "recovery".into(),
                severity: "error".into(),
                phase: "lease_reconciliation".into(),
                code: "mutation_lease_abandoned".into(),
                item_id: Some(plan_item_id),
                sequence_no: u64::try_from(sequence_no).ok(),
                path_role: None,
                message_key: "diagnostic_recovery_required".into(),
                payload: serde_json::json!({
                    "cause_chain": ["mutation_lease_abandoned"],
                    "direction": direction,
                    "journal_state": state,
                    "operation_id": operation_id,
                }),
                contains_sensitive_path: false,
                protected: true,
            },
        );
    }
    reconcile_terminal_journal_summaries(&transaction, current_time)?;
    let terminal_apply_attempts = {
        let mut statement = transaction
            .prepare(
                "SELECT execution.id
                   FROM execution_runs execution
                  WHERE execution.mode='apply'
                    AND execution.status IN ('running','recovery_required')
                    AND EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=execution.id AND journal.direction='apply'
                    )
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         JOIN mutation_leases lease
                           ON lease.fencing_token=journal.fencing_token
                         WHERE journal.attempt_id=execution.id AND journal.direction='apply'
                    )
                  ORDER BY execution.id",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    for execution_id in terminal_apply_attempts {
        persist_terminal_apply_outcome(&transaction, &execution_id, current_time)?;
    }
    let terminal_rollback_attempts = {
        let mut statement = transaction
            .prepare(
                "SELECT rollback.id
                   FROM rollback_runs rollback
                  WHERE rollback.mode='rollback'
                    AND rollback.status IN ('running','recovery_required')
                    AND EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=rollback.id AND journal.direction='rollback'
                    )
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         JOIN mutation_leases lease
                           ON lease.fencing_token=journal.fencing_token
                         WHERE journal.attempt_id=rollback.id AND journal.direction='rollback'
                    )
                  ORDER BY rollback.id",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    for rollback_id in terminal_rollback_attempts {
        persist_terminal_rollback_outcome(&transaction, &rollback_id, current_time)?;
    }
    transaction
        .execute(
            "UPDATE recovery_runs
                SET status='completed',finished_at=COALESCE(finished_at,?1),error=NULL
              WHERE status='running' AND EXISTS(
                    SELECT 1 FROM operation_journal journal
                     WHERE journal.id=recovery_runs.operation_id
                       AND journal.state IN ('completed','failed')
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE execution_runs
                SET status='recovery_required',finished_at=COALESCE(finished_at,?1)
              WHERE status='running' AND EXISTS (
                    SELECT 1 FROM operation_journal journal
                     WHERE journal.attempt_id=execution_runs.id
                       AND journal.direction='apply'
                       AND journal.state NOT IN ('completed','failed')
                       AND instr(COALESCE(journal.error,''),'mutation_lease_abandoned')>0
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE rollback_runs
                SET status='recovery_required',finished_at=COALESCE(finished_at,?1)
              WHERE status='running' AND EXISTS (
                    SELECT 1 FROM operation_journal journal
                     WHERE journal.attempt_id=rollback_runs.id
                       AND journal.direction='rollback'
                       AND journal.state NOT IN ('completed','failed')
                       AND instr(COALESCE(journal.error,''),'mutation_lease_abandoned')>0
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE recovery_runs
                SET status='recovery_required',finished_at=COALESCE(finished_at,?1),
                    error=COALESCE(error,'mutation_lease_abandoned')
              WHERE status='running' AND EXISTS (
                    SELECT 1 FROM operation_journal journal
                     WHERE journal.id=recovery_runs.operation_id
                       AND journal.state NOT IN ('completed','failed')
                       AND instr(COALESCE(journal.error,''),'mutation_lease_abandoned')>0
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE workflow_attempts
                SET status='recovery_required',finished_at=COALESCE(finished_at,?1),
                    error_code=COALESCE(error_code,'mutation_lease_abandoned')
              WHERE status='running' AND (
                    EXISTS(SELECT 1 FROM execution_runs execution
                            WHERE execution.id=workflow_attempts.id
                              AND execution.status='recovery_required')
                 OR EXISTS(SELECT 1 FROM rollback_runs rollback
                            WHERE rollback.id=workflow_attempts.id
                              AND rollback.status='recovery_required')
                 OR EXISTS(SELECT 1 FROM recovery_runs recovery
                            WHERE recovery.id=workflow_attempts.id
                              AND recovery.status='recovery_required')
              )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE workflow_attempts
                SET status=(SELECT execution.status FROM execution_runs execution
                             WHERE execution.id=workflow_attempts.id),
                    finished_at=COALESCE(finished_at,?1),
                    success_count=(SELECT execution.success_count FROM execution_runs execution
                                   WHERE execution.id=workflow_attempts.id),
                    skipped_count=(SELECT execution.skipped_count FROM execution_runs execution
                                   WHERE execution.id=workflow_attempts.id),
                    failed_count=(SELECT execution.failed_count FROM execution_runs execution
                                  WHERE execution.id=workflow_attempts.id),
                    error_code=CASE
                      WHEN (SELECT execution.status FROM execution_runs execution
                            WHERE execution.id=workflow_attempts.id)='partial'
                      THEN COALESCE(error_code,'journal_summary_reconciled')
                      ELSE error_code END
              WHERE status='running' AND EXISTS(
                    SELECT 1 FROM execution_runs execution
                     WHERE execution.id=workflow_attempts.id
                       AND execution.status<>'running'
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE workflow_attempts
                SET status=(SELECT rollback.status FROM rollback_runs rollback
                             WHERE rollback.id=workflow_attempts.id),
                    finished_at=COALESCE(finished_at,?1),
                    success_count=(SELECT rollback.success_count FROM rollback_runs rollback
                                   WHERE rollback.id=workflow_attempts.id),
                    skipped_count=(SELECT rollback.skipped_count FROM rollback_runs rollback
                                   WHERE rollback.id=workflow_attempts.id),
                    failed_count=(SELECT rollback.failed_count FROM rollback_runs rollback
                                  WHERE rollback.id=workflow_attempts.id),
                    error_code=CASE
                      WHEN (SELECT rollback.status FROM rollback_runs rollback
                            WHERE rollback.id=workflow_attempts.id)='partial'
                      THEN COALESCE(error_code,'journal_summary_reconciled')
                      ELSE error_code END
              WHERE status='running' AND EXISTS(
                    SELECT 1 FROM rollback_runs rollback
                     WHERE rollback.id=workflow_attempts.id
                       AND rollback.status<>'running'
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE workflow_attempts
                SET status=(SELECT recovery.status FROM recovery_runs recovery
                             WHERE recovery.id=workflow_attempts.id),
                    finished_at=COALESCE(finished_at,?1),
                    success_count=CASE
                      WHEN (SELECT recovery.status FROM recovery_runs recovery
                            WHERE recovery.id=workflow_attempts.id)='completed' THEN 1 ELSE 0 END,
                    failed_count=CASE
                      WHEN (SELECT recovery.status FROM recovery_runs recovery
                            WHERE recovery.id=workflow_attempts.id)='completed' THEN 0 ELSE 1 END,
                    error_code=COALESCE(
                        error_code,
                        (SELECT recovery.error FROM recovery_runs recovery
                          WHERE recovery.id=workflow_attempts.id)
                    )
              WHERE status='running' AND EXISTS(
                    SELECT 1 FROM recovery_runs recovery
                     WHERE recovery.id=workflow_attempts.id
                       AND recovery.status<>'running'
                )",
            params![current_time],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

const LOCAL_ATTEMPT_ACTOR: &str = "local_application";

#[allow(clippy::too_many_arguments)]
fn insert_workflow_attempt(
    connection: &Connection,
    id: &str,
    root_scan_id: &str,
    logical_run_id: &str,
    kind: &str,
    parent_attempt_id: Option<&str>,
    actor: &str,
    started_at: i64,
) -> Result<(), String> {
    let changed = connection
        .execute(
            "INSERT INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at
             )
             SELECT ?1,?2,?3,?4,
                    COALESCE(MAX(previous.attempt_no),0)+1,?5,
                    (SELECT predecessor.id FROM workflow_attempts predecessor
                      WHERE predecessor.logical_run_id=?3 AND predecessor.kind=?4
                      ORDER BY predecessor.attempt_no DESC LIMIT 1),
                    ?6,'running',?7
               FROM workflow_attempts previous
              WHERE previous.logical_run_id=?3 AND previous.kind=?4",
            params![
                id,
                root_scan_id,
                logical_run_id,
                kind,
                parent_attempt_id,
                actor,
                started_at
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == 1 {
        Ok(())
    } else {
        Err("workflow_attempt_begin_rejected".into())
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_workflow_attempt(
    connection: &Connection,
    id: &str,
    kind: &str,
    status: RunStatus,
    finished_at: i64,
    success: u64,
    skipped: u64,
    failed: u64,
    explicit_error: Option<&str>,
) -> Result<(), String> {
    if status == RunStatus::Running {
        return Err("workflow_attempt_terminal_status_required".into());
    }
    let generated_error = match status {
        RunStatus::Failed => Some(format!("{kind}_failed")),
        RunStatus::RecoveryRequired => Some(format!("{kind}_recovery_required")),
        RunStatus::Partial => Some(format!("{kind}_partial")),
        RunStatus::Cancelled => Some(format!("{kind}_cancelled")),
        RunStatus::Running | RunStatus::Completed | RunStatus::Archived => None,
    };
    let error_code = explicit_error.or(generated_error.as_deref());
    let changed = connection
        .execute(
            "UPDATE workflow_attempts
                SET status=?2,finished_at=?3,success_count=?4,skipped_count=?5,
                    failed_count=?6,error_code=?7
              WHERE id=?1 AND status='running'",
            params![
                id,
                status.as_str(),
                finished_at,
                i64::try_from(success).map_err(|_| "workflow_attempt_success_count_invalid")?,
                i64::try_from(skipped).map_err(|_| "workflow_attempt_skipped_count_invalid")?,
                i64::try_from(failed).map_err(|_| "workflow_attempt_failed_count_invalid")?,
                error_code
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == 1 {
        if let Some(error_code) = error_code {
            let diagnostic_code = stable_diagnostic_code(error_code, "workflow_failed");
            let logical_run_id: Option<String> = connection
                .query_row(
                    "SELECT logical_run_id FROM workflow_attempts WHERE id=?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);
            diagnostics::insert_diagnostic_best_effort(
                connection,
                DiagnosticEventInput {
                    run_id: logical_run_id.or_else(|| Some(id.to_owned())),
                    attempt_id: Some(id.to_owned()),
                    class: if status == RunStatus::RecoveryRequired {
                        "recovery".into()
                    } else {
                        "diagnostic".into()
                    },
                    severity: if matches!(status, RunStatus::Partial | RunStatus::Cancelled) {
                        "warning".into()
                    } else {
                        "error".into()
                    },
                    phase: kind.to_owned(),
                    code: diagnostic_code.clone(),
                    item_id: None,
                    sequence_no: None,
                    path_role: None,
                    message_key: if status == RunStatus::RecoveryRequired {
                        "workflow_recovery_required".into()
                    } else {
                        "workflow_failed".into()
                    },
                    payload: serde_json::json!({
                        "status": status.as_str(),
                        "counts": {"success": success, "skipped": skipped, "failed": failed},
                        "cause_chain": diagnostic_cause_chain(error_code),
                    }),
                    contains_sensitive_path: false,
                    protected: status == RunStatus::RecoveryRequired,
                },
            );
        }
        Ok(())
    } else {
        Err("workflow_attempt_not_running".into())
    }
}

fn stable_diagnostic_code(value: &str, fallback: &str) -> String {
    value
        .split(':')
        .find(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
        .unwrap_or(fallback)
        .to_owned()
}

fn diagnostic_cause_chain(value: &str) -> Vec<serde_json::Value> {
    let mut causes = value
        .split(':')
        .filter(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
        .take(8)
        .map(|code| serde_json::json!({"code": code}))
        .collect::<Vec<_>>();
    if causes.is_empty() {
        causes.push(serde_json::json!({"code": "internal_error"}));
    }
    causes
}

fn security_rejection_code(code: &str) -> bool {
    code.starts_with("reparse_")
        || code.starts_with("invalid_path_")
        || code.contains("outside_root")
        || code.contains("path_traversal")
        || code.contains("archived_read_only")
        || code.contains("authorization")
        || code.contains("capability")
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn disposition_name(disposition: ExecutionDisposition) -> &'static str {
    match disposition {
        ExecutionDisposition::Executable => "executable",
        ExecutionDisposition::Skip => "skip",
        ExecutionDisposition::Blocked => "blocked",
    }
}

fn persisted_conflict_group_id(plan_id: &str, group_id: Uuid) -> String {
    let mut digest = Sha256::new();
    digest.update(b"music-folder-plan-conflict-group");
    digest.update((plan_id.len() as u64).to_le_bytes());
    digest.update(plan_id.as_bytes());
    digest.update(group_id.as_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Keep the stored identifier parseable as an RFC-compatible UUID while
    // scoping Core's deterministic diagnostic group to one persisted Plan.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn parse_disposition(value: &str) -> Result<ExecutionDisposition, String> {
    match value {
        "executable" => Ok(ExecutionDisposition::Executable),
        "skip" => Ok(ExecutionDisposition::Skip),
        "blocked" => Ok(ExecutionDisposition::Blocked),
        _ => Err(format!("plan_execution_disposition_unknown:{value}")),
    }
}

fn current_naming_rules(
    rules_schema_version: i64,
    rules_json: &str,
    legacy_error: &str,
) -> Result<NamingRules, String> {
    let version = u32::try_from(rules_schema_version).map_err(|_| legacy_error.to_owned())?;
    require_current_naming_rules_schema(version).map_err(|_| legacy_error.to_owned())?;
    let naming: NamingRules =
        serde_json::from_str(rules_json).map_err(|_| "plan_rules_json_invalid".to_owned())?;
    if let Some(issue) = validate_naming_rules(&naming).first() {
        return Err(format!("plan_rules_invalid:{}", issue.code));
    }
    Ok(naming)
}

fn serialized_plan_issues(item: &PlanItem) -> Result<String, String> {
    serde_json::to_string(&item.issues).map_err(|error| error.to_string())
}

fn plan_build_persistence_context(
    connection: &Connection,
    build_id: &str,
) -> Result<(PathBuf, NamingRules), String> {
    let context: Option<(String, Vec<u8>, String, i64)> = connection
        .query_row(
            "SELECT plan.target_root_encoding,plan.target_root_blob,
                    plan.rules_json,plan.rules_schema_version
               FROM plan_builds build
               JOIN plan_runs plan ON plan.id=build.plan_id
              WHERE build.id=?1 AND build.status='building' AND plan.status='running'",
            params![build_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let (encoding, raw, rules_json, rules_schema_version) =
        context.ok_or_else(|| "plan_build_not_active".to_owned())?;
    let target_root = decode_path(&encoding, &raw)?;
    let naming = current_naming_rules(
        rules_schema_version,
        &rules_json,
        "legacy_plan_non_executable",
    )?;
    Ok((target_root, naming))
}

fn reject_archived(connection: &Connection, query: &str, workflow_id: &str) -> Result<(), String> {
    let archived: Option<i64> = connection
        .query_row(query, params![workflow_id], |row| row.get(0))
        .optional()
        .map_err(|error| error.to_string())?;
    if archived.is_some_and(|value| value != 0) {
        diagnostics::insert_diagnostic_best_effort(
            connection,
            DiagnosticEventInput {
                run_id: Some(workflow_id.to_owned()),
                attempt_id: None,
                class: "audit".into(),
                severity: "warning".into(),
                phase: "authorization".into(),
                code: "workflow_archived_read_only".into(),
                item_id: None,
                sequence_no: None,
                path_role: None,
                message_key: "security_rejected".into(),
                payload: serde_json::json!({
                    "cause_chain": [{"code": "workflow_archived_read_only"}],
                }),
                contains_sensitive_path: false,
                protected: true,
            },
        );
        Err("workflow_archived_read_only".into())
    } else {
        Ok(())
    }
}

fn ensure_scan_mutable(connection: &Connection, scan_id: &str) -> Result<(), String> {
    reject_archived(
        connection,
        "SELECT archived_at IS NOT NULL FROM scan_runs WHERE id=?1",
        scan_id,
    )
}

fn ensure_plan_mutable(connection: &Connection, plan_id: &str) -> Result<(), String> {
    reject_archived(
        connection,
        "SELECT (plan.archived_at IS NOT NULL OR scan.archived_at IS NOT NULL)
           FROM plan_runs plan JOIN scan_runs scan ON scan.id=plan.scan_id
          WHERE plan.id=?1",
        plan_id,
    )
}

fn ensure_execution_mutable(connection: &Connection, execution_id: &str) -> Result<(), String> {
    reject_archived(
        connection,
        "SELECT (execution.archived_at IS NOT NULL OR plan.archived_at IS NOT NULL
                 OR scan.archived_at IS NOT NULL)
           FROM execution_runs execution
           JOIN plan_runs plan ON plan.id=execution.plan_id
           JOIN scan_runs scan ON scan.id=plan.scan_id
          WHERE execution.id=?1",
        execution_id,
    )
}

fn ensure_rollback_mutable(connection: &Connection, rollback_id: &str) -> Result<(), String> {
    reject_archived(
        connection,
        "SELECT (rollback.archived_at IS NOT NULL OR execution.archived_at IS NOT NULL
                 OR plan.archived_at IS NOT NULL OR scan.archived_at IS NOT NULL)
           FROM rollback_runs rollback
           JOIN execution_runs execution ON execution.id=rollback.execution_id
           JOIN plan_runs plan ON plan.id=execution.plan_id
           JOIN scan_runs scan ON scan.id=plan.scan_id
          WHERE rollback.id=?1",
        rollback_id,
    )
}

fn ensure_recovery_operation_mutable(
    connection: &Connection,
    operation_id: &str,
) -> Result<(), String> {
    reject_archived(
        connection,
        "SELECT COALESCE(MAX(archived),0) FROM (
             SELECT (execution.archived_at IS NOT NULL OR plan.archived_at IS NOT NULL
                     OR scan.archived_at IS NOT NULL) archived
               FROM operation_journal journal
               JOIN execution_runs execution
                 ON journal.direction='apply' AND execution.id=journal.attempt_id
               JOIN plan_runs plan ON plan.id=execution.plan_id
               JOIN scan_runs scan ON scan.id=plan.scan_id
              WHERE journal.id=?1
             UNION ALL
             SELECT (rollback.archived_at IS NOT NULL OR execution.archived_at IS NOT NULL
                     OR plan.archived_at IS NOT NULL OR scan.archived_at IS NOT NULL) archived
               FROM operation_journal journal
               JOIN rollback_runs rollback
                 ON journal.direction='rollback' AND rollback.id=journal.attempt_id
               JOIN execution_runs execution ON execution.id=rollback.execution_id
               JOIN plan_runs plan ON plan.id=execution.plan_id
               JOIN scan_runs scan ON scan.id=plan.scan_id
              WHERE journal.id=?1
         )",
        operation_id,
    )
}

fn ensure_recovery_run_mutable(connection: &Connection, recovery_id: &str) -> Result<(), String> {
    let operation_id: Option<String> = connection
        .query_row(
            "SELECT operation_id FROM recovery_runs WHERE id=?1",
            params![recovery_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(operation_id) = operation_id {
        ensure_recovery_operation_mutable(connection, &operation_id)?;
    }
    Ok(())
}

fn load_scan_snapshot_items(
    connection: &Connection,
    scan_id: &str,
) -> Result<Vec<ScannedFile>, String> {
    let mut statement = connection
        .prepare(
            "SELECT path_encoding,path_blob,size_bytes,mtime_ns,content_sha256,
                    file_identity,fingerprint_version,metadata_json,metadata_status,kind
               FROM scan_items
              WHERE scan_id=?1
              ORDER BY path_encoding,path_blob",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![scan_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    rows.into_iter()
        .map(
            |(
                path_encoding,
                path_blob,
                size_bytes,
                mtime_ns,
                content_sha256,
                file_identity,
                fingerprint_version,
                metadata_json,
                metadata_status,
                kind,
            )| {
                let metadata = match metadata_status.as_str() {
                    "ok" => Some(
                        serde_json::from_str(
                            metadata_json
                                .as_deref()
                                .ok_or_else(|| "scan_snapshot_metadata_missing".to_string())?,
                        )
                        .map_err(|error| format!("scan_snapshot_metadata_invalid:{error}"))?,
                    ),
                    "error" | "not_applicable" => None,
                    value => return Err(format!("scan_snapshot_status_unknown:{value}")),
                };
                Ok(ScannedFile {
                    id: Uuid::new_v4(),
                    path: decode_path(&path_encoding, &path_blob)?,
                    fingerprint: FileFingerprint {
                        size_bytes: u64::try_from(size_bytes)
                            .map_err(|_| "scan_snapshot_size_invalid".to_string())?,
                        mtime_ns: mtime_ns
                            .parse()
                            .map_err(|_| "scan_snapshot_mtime_invalid".to_string())?,
                        content_sha256,
                        file_identity,
                        version: u16::try_from(fingerprint_version)
                            .map_err(|_| "scan_snapshot_fingerprint_version_invalid".to_string())?,
                    },
                    metadata,
                    kind: match kind.as_str() {
                        "image" => FileKind::Image,
                        "music" => FileKind::Music,
                        value => return Err(format!("scan_snapshot_kind_unknown:{value}")),
                    },
                })
            },
        )
        .collect()
}

fn insert_published_plan_item(
    transaction: &Transaction<'_>,
    plan_id: &str,
    staged: &StagedPlanItem,
) -> Result<(), String> {
    let item = &staged.item;
    let group_id = item
        .conflict_group_id
        .map(|value| persisted_conflict_group_id(plan_id, value));
    let (source_encoding, source_blob) = encode_path(&item.file.path);
    let (target_encoding, target_blob) = item
        .target
        .as_deref()
        .map(encode_path)
        .map_or((None, None), |(encoding, blob)| {
            (Some(encoding), Some(blob))
        });
    let metadata_json = item
        .file
        .metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| error.to_string())?;
    let issues_json = serialized_plan_issues(item)?;
    transaction
        .execute(
            "INSERT INTO plan_items(
                 id,plan_id,ordinal,source_path,source_path_encoding,source_path_blob,
                 target_path,target_path_encoding,target_path_blob,target_origin,
                 conflict_group_id,action,execution_disposition,risk,reason,
                 issues_schema_version,issues_json,source_size_bytes,source_mtime_ns,
                 source_file_identity,source_identity_version,source_content_hash,
                 source_hash_algorithm,source_hash_version,source_kind,source_metadata_json
             ) VALUES(
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18,?19,?20,?21,?22,?23,?24,?25,?26
             )",
            params![
                item.id.to_string(),
                plan_id,
                i64::try_from(item.ordinal).map_err(|_| "plan_ordinal_invalid")?,
                item.file.path.to_string_lossy(),
                source_encoding,
                source_blob,
                item.target
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                target_encoding,
                target_blob,
                staged.change_origin.as_deref().unwrap_or("rule"),
                group_id,
                plan_action_name(item.action),
                disposition_name(item.disposition),
                risk_name(item.risk),
                item.reason,
                i64::from(PLAN_ISSUES_SCHEMA_VERSION),
                issues_json,
                i64::try_from(item.file.fingerprint.size_bytes)
                    .map_err(|_| "plan_source_size_invalid")?,
                item.file.fingerprint.mtime_ns.to_string(),
                item.file
                    .fingerprint
                    .file_identity
                    .as_deref()
                    .map(str::as_bytes),
                item.file
                    .fingerprint
                    .file_identity
                    .as_ref()
                    .map(|_| i64::from(item.file.fingerprint.version)),
                item.file
                    .fingerprint
                    .content_sha256
                    .as_deref()
                    .map(str::as_bytes),
                item.file
                    .fingerprint
                    .content_sha256
                    .as_ref()
                    .map(|_| "sha256"),
                item.file
                    .fingerprint
                    .content_sha256
                    .as_ref()
                    .map(|_| i64::from(item.file.fingerprint.version)),
                match item.file.kind {
                    FileKind::Music => "music",
                    FileKind::Image => "image",
                },
                metadata_json,
            ],
        )
        .map_err(|error| error.to_string())?;

    if let Some(group_id) = &group_id {
        if let Some(target) = &item.target {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO plan_conflict_groups(
                         id,plan_id,kind,normalized_target_path,target_path
                     ) VALUES(?1,?2,'plan_items',?3,?4)",
                    params![
                        group_id,
                        plan_id,
                        windows_path_key(target),
                        target.to_string_lossy()
                    ],
                )
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id)
                     VALUES(?1,?2)",
                    params![group_id, item.id.to_string()],
                )
                .map_err(|error| error.to_string())?;
        } else if !item.conflict_candidates.is_empty() {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO plan_conflict_groups(
                         id,plan_id,kind,normalized_target_path,target_path
                     ) VALUES(?1,?2,'image_destination','','')",
                    params![group_id, plan_id],
                )
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id)
                     VALUES(?1,?2)",
                    params![group_id, item.id.to_string()],
                )
                .map_err(|error| error.to_string())?;
            for (index, candidate) in item.conflict_candidates.iter().enumerate() {
                let ordinal = i64::try_from(index + 1)
                    .map_err(|_| "plan_conflict_candidate_ordinal_invalid")?;
                let (candidate_encoding, candidate_blob) = encode_path(&candidate.target_directory);
                transaction
                    .execute(
                        "INSERT INTO plan_conflict_candidates(
                             conflict_group_id,ordinal,target_path,
                             target_path_encoding,target_path_blob
                         ) VALUES(?1,?2,?3,?4,?5)",
                        params![
                            group_id,
                            ordinal,
                            candidate.target_directory.to_string_lossy(),
                            candidate_encoding,
                            candidate_blob,
                        ],
                    )
                    .map_err(|error| error.to_string())?;
                for member in &candidate.music_item_ids {
                    transaction
                        .execute(
                            "INSERT INTO plan_conflict_candidate_members(
                                 conflict_group_id,candidate_ordinal,plan_item_id
                             ) VALUES(?1,?2,?3)",
                            params![group_id, ordinal, member.to_string()],
                        )
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
    if let Some(parent_item_id) = staged.parent_plan_item_id.as_deref() {
        transaction
            .execute(
                "INSERT INTO plan_revision_changes(
                     child_plan_id,parent_plan_item_id,child_plan_item_id,origin,reason
                 ) VALUES(?1,?2,?3,?4,?5)",
                params![
                    plan_id,
                    parent_item_id,
                    item.id.to_string(),
                    staged.change_origin.as_deref().unwrap_or("rule"),
                    staged.change_reason.as_deref().unwrap_or("unchanged")
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn load_persisted_plan_page(
    connection: &Connection,
    plan_id: &str,
    after_ordinal: u64,
    limit: usize,
) -> Result<Vec<StagedPlanItem>, String> {
    type RawPlanItem = (
        String,
        i64,
        String,
        Vec<u8>,
        Option<String>,
        Option<Vec<u8>>,
        String,
        String,
        String,
        Option<String>,
        i64,
        String,
        Option<i64>,
        Option<String>,
        Option<Vec<u8>>,
        Option<i64>,
        Option<Vec<u8>>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let mut statement = connection
        .prepare(
            "SELECT id,ordinal,source_path_encoding,source_path_blob,
                    target_path_encoding,target_path_blob,action,execution_disposition,
                    risk,reason,issues_schema_version,issues_json,
                    source_size_bytes,source_mtime_ns,source_file_identity,
                    source_identity_version,source_content_hash,source_hash_version,
                    source_kind,source_metadata_json,conflict_group_id
               FROM plan_items
              WHERE plan_id=?1 AND ordinal>?2
              ORDER BY ordinal
              LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![
                plan_id,
                i64::try_from(after_ordinal).map_err(|_| "plan_ordinal_invalid")?,
                i64::try_from(limit.max(1)).map_err(|_| "plan_page_limit_invalid")?
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                    row.get(19)?,
                    row.get(20)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<RawPlanItem>, _>>()
        .map_err(|error| error.to_string())?;
    let mut items = Vec::with_capacity(rows.len());
    for (
        id,
        ordinal,
        source_encoding,
        source_blob,
        target_encoding,
        target_blob,
        action,
        disposition,
        risk,
        reason,
        issues_schema_version,
        issues_json,
        source_size,
        source_mtime,
        source_identity,
        source_identity_version,
        source_hash,
        source_hash_version,
        source_kind,
        source_metadata_json,
        conflict_group_id,
    ) in rows
    {
        let target = match (target_encoding, target_blob) {
            (Some(encoding), Some(blob)) => Some(decode_path(&encoding, &blob)?),
            (None, None) => None,
            _ => return Err("plan_target_path_encoding_incomplete".into()),
        };
        let group_id = conflict_group_id
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| "plan_conflict_group_id_invalid")?;
        let mut candidates = Vec::new();
        if let Some(group_id) = conflict_group_id.as_deref() {
            let mut candidate_statement = connection
                .prepare(
                    "SELECT ordinal,target_path_encoding,target_path_blob
                       FROM plan_conflict_candidates
                      WHERE conflict_group_id=?1 ORDER BY ordinal",
                )
                .map_err(|error| error.to_string())?;
            let candidate_rows = candidate_statement
                .query_map(params![group_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            for (candidate_ordinal, target_encoding, target_blob) in candidate_rows {
                let mut member_statement = connection
                    .prepare(
                        "SELECT plan_item_id FROM plan_conflict_candidate_members
                          WHERE conflict_group_id=?1 AND candidate_ordinal=?2
                          ORDER BY plan_item_id",
                    )
                    .map_err(|error| error.to_string())?;
                let members = member_statement
                    .query_map(params![group_id, candidate_ordinal], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(|error| error.to_string())?
                    .map(|value| {
                        value.map_err(|error| error.to_string()).and_then(|value| {
                            Uuid::parse_str(&value)
                                .map_err(|_| "plan_candidate_member_id_invalid".to_string())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                candidates.push(music_folder_core::PlanConflictCandidate {
                    target_directory: decode_path(&target_encoding, &target_blob)?,
                    music_item_ids: members,
                });
            }
        }
        let size_bytes =
            u64::try_from(source_size.ok_or_else(|| "plan_source_size_missing".to_string())?)
                .map_err(|_| "plan_source_size_invalid")?;
        let mtime_ns = source_mtime
            .ok_or_else(|| "plan_source_mtime_missing".to_string())?
            .parse::<i128>()
            .map_err(|_| "plan_source_mtime_invalid")?;
        let fingerprint_version =
            u16::try_from(source_hash_version.or(source_identity_version).unwrap_or(1))
                .map_err(|_| "plan_source_fingerprint_version_invalid")?;
        let action = match action.as_str() {
            "move" => PlanAction::Move,
            "skip" => PlanAction::Skip,
            value => return Err(format!("plan_action_unknown:{value}")),
        };
        let risk = match risk.as_str() {
            "none" => Risk::None,
            "invalid_target" => Risk::InvalidTarget,
            "path_too_long" => Risk::PathTooLong,
            "conflict" => Risk::Conflict,
            "metadata_missing" => Risk::MetadataMissing,
            value => return Err(format!("plan_risk_unknown:{value}")),
        };
        if issues_schema_version != i64::from(PLAN_ISSUES_SCHEMA_VERSION) {
            return Err("legacy_plan_issues_unsupported".into());
        }
        let disposition = parse_disposition(&disposition)?;
        let issues: Vec<PlanIssue> = serde_json::from_str(&issues_json)
            .map_err(|_| "plan_issues_json_invalid".to_owned())?;
        items.push(StagedPlanItem {
            item: PlanItem {
                id: Uuid::parse_str(&id).map_err(|_| "plan_item_id_invalid")?,
                conflict_group_id: group_id,
                ordinal: u64::try_from(ordinal).map_err(|_| "plan_ordinal_invalid")?,
                file: ScannedFile {
                    id: Uuid::nil(),
                    path: decode_path(&source_encoding, &source_blob)?,
                    fingerprint: FileFingerprint {
                        size_bytes,
                        mtime_ns,
                        content_sha256: source_hash
                            .map(String::from_utf8)
                            .transpose()
                            .map_err(|_| "plan_source_hash_invalid_utf8")?,
                        file_identity: source_identity
                            .map(String::from_utf8)
                            .transpose()
                            .map_err(|_| "plan_source_identity_invalid_utf8")?,
                        version: fingerprint_version,
                    },
                    metadata: source_metadata_json
                        .map(|json| serde_json::from_str(&json))
                        .transpose()
                        .map_err(|error| format!("plan_source_metadata_invalid:{error}"))?,
                    kind: if source_kind.as_deref() == Some("image") {
                        FileKind::Image
                    } else {
                        FileKind::Music
                    },
                },
                target,
                action,
                disposition,
                risk,
                reason,
                issues,
                conflict_candidates: candidates,
            },
            parent_plan_item_id: Some(id),
            change_origin: None,
            change_reason: None,
        });
    }
    Ok(items)
}

impl ScanStore for SqliteScanStore {
    fn previous_metadata(
        &self,
        path: &Path,
        fp: &FileFingerprint,
    ) -> Result<Option<TrackMetadata>, String> {
        let Some(content_sha256) = fp.content_sha256.as_deref() else {
            return Ok(None);
        };
        let Some(file_identity) = fp.file_identity.as_deref() else {
            return Ok(None);
        };
        let (path_encoding, path_blob) = encode_path(path);
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let row: Option<String> = conn
            .query_row(
                "SELECT metadata_json
               FROM metadata_cache_entries
              WHERE path_encoding=?1 AND path_blob=?2
                AND size_bytes=?3 AND mtime_ns=?4 AND content_sha256=?5
                AND file_identity=?6 AND fingerprint_version=?7
                AND fingerprint_algorithm=?8 AND kind='music'
                AND reader_id=?9 AND reader_version=?10 AND schema_version=?11
                AND reader_config_hash=?12 AND path_normalization_version=?13
                AND metadata_status='ok'
              ORDER BY id DESC
              LIMIT 1",
                params![
                    path_encoding,
                    path_blob,
                    fp.size_bytes as i64,
                    fp.mtime_ns.to_string(),
                    content_sha256,
                    file_identity,
                    i64::from(fp.version),
                    METADATA_FINGERPRINT_ALGORITHM,
                    METADATA_READER_ID,
                    METADATA_READER_VERSION,
                    METADATA_SCHEMA_VERSION,
                    METADATA_READER_CONFIG_HASH,
                    i64::from(WINDOWS_PATH_KEY_VERSION),
                ],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        row.map(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
            .transpose()
    }
    fn begin_scan(&self, source: &Path) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let (encoding, raw) = encode_path(source);
        let started_at = now();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        transaction.execute("INSERT INTO scan_runs(id,source_root,source_root_encoding,source_root_blob,status,started_at) VALUES(?1,?2,?3,?4,'running',?5)",params![id,source.to_string_lossy(),encoding,raw,started_at]).map_err(|e|e.to_string())?;
        insert_workflow_attempt(
            &transaction,
            &id,
            &id,
            &id,
            "scan",
            None,
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }
    fn save_batch(&self, scan_id: &str, files: &[ScannedFile]) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let mut next_ordinal: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(ordinal),-1)+1 FROM scan_items WHERE scan_id=?1",
                params![scan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        for f in files {
            let (path_encoding, path_blob) = encode_path(&f.path);
            let path_key = windows_path_key(&f.path);
            let json = f
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| e.to_string())?;
            let kind = if f.kind == FileKind::Image {
                "image"
            } else {
                "music"
            };
            let snapshot_status = if f.metadata.is_some() {
                "ok"
            } else if f.kind == FileKind::Image {
                "not_applicable"
            } else {
                "error"
            };
            tx.execute(
                "INSERT INTO scan_items(
                     scan_id,ordinal,path,path_encoding,path_blob,path_key_version,path_key,
                     size_bytes,mtime_ns,content_sha256,file_identity,fingerprint_version,
                     metadata_json,metadata_status,kind
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    scan_id,
                    next_ordinal,
                    f.path.to_string_lossy(),
                    path_encoding,
                    path_blob,
                    i64::from(WINDOWS_PATH_KEY_VERSION),
                    path_key,
                    f.fingerprint.size_bytes as i64,
                    f.fingerprint.mtime_ns.to_string(),
                    f.fingerprint.content_sha256.as_deref(),
                    f.fingerprint.file_identity.as_deref(),
                    f.fingerprint.version as i64,
                    json.as_deref(),
                    snapshot_status,
                    kind
                ],
            )
            .map_err(|e| e.to_string())?;
            next_ordinal = next_ordinal
                .checked_add(1)
                .ok_or_else(|| "scan_item_ordinal_overflow".to_string())?;
            tx.execute(
                "INSERT INTO metadata_cache_entries(
                     path,path_encoding,path_blob,size_bytes,mtime_ns,content_sha256,
                     file_identity,fingerprint_version,fingerprint_algorithm,
                     reader_id,reader_version,schema_version,reader_config_hash,
                     path_normalization_version,metadata_json,metadata_status,kind,
                     scan_id,created_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                          ?15,?16,?17,?18,?19)",
                params![
                    f.path.to_string_lossy(),
                    path_encoding,
                    path_blob,
                    f.fingerprint.size_bytes as i64,
                    f.fingerprint.mtime_ns.to_string(),
                    f.fingerprint.content_sha256.as_deref(),
                    f.fingerprint.file_identity.as_deref(),
                    f.fingerprint.version as i64,
                    METADATA_FINGERPRINT_ALGORITHM,
                    METADATA_READER_ID,
                    METADATA_READER_VERSION,
                    METADATA_SCHEMA_VERSION,
                    METADATA_READER_CONFIG_HASH,
                    i64::from(WINDOWS_PATH_KEY_VERSION),
                    json.as_deref(),
                    if f.metadata.is_some() { "ok" } else { "error" },
                    kind,
                    scan_id,
                    now()
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
    fn finish_scan(&self, scan_id: &str, status: RunStatus, warnings: u64) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let snapshot_hash = if status == RunStatus::Completed {
            let items = load_scan_snapshot_items(&transaction, scan_id)?;
            Some(music_folder_core::scan_snapshot_hash(&items))
        } else {
            None
        };
        let finished_at = now();
        let changed = transaction
            .execute(
                "UPDATE scan_runs
                    SET status=?2,finished_at=?3,warning_count=?4,
                        snapshot_hash=?5,snapshot_version=?6
                  WHERE id=?1 AND status='running'",
                params![
                    scan_id,
                    status.as_str(),
                    finished_at,
                    warnings as i64,
                    snapshot_hash,
                    music_folder_core::SCAN_SNAPSHOT_VERSION as i64
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("scan_not_running".into());
        }
        let item_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM scan_items WHERE scan_id=?1",
                params![scan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        finish_workflow_attempt(
            &transaction,
            scan_id,
            "scan",
            status,
            finished_at,
            u64::try_from(item_count).map_err(|_| "scan_item_count_invalid")?,
            0,
            warnings,
            None,
        )?;
        transaction.commit().map_err(|error| error.to_string())
    }
    fn save_scan_warning(&self, scan_id: &str, warning: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        connection
            .execute(
                "INSERT INTO scan_warnings(id,scan_id,warning,created_at) VALUES(?1,?2,?3,?4)",
                params![Uuid::new_v4().to_string(), scan_id, warning, now()],
            )
            .map_err(|error| error.to_string())?;
        let sequence_no = connection
            .query_row(
                "SELECT COUNT(*) FROM scan_warnings WHERE scan_id=?1",
                params![scan_id],
                |row| row.get::<_, i64>(0),
            )
            .ok()
            .and_then(|value| u64::try_from(value).ok());
        let code = stable_diagnostic_code(warning, "scan_warning");
        let security = security_rejection_code(&code);
        diagnostics::insert_diagnostic_best_effort(
            &connection,
            DiagnosticEventInput {
                run_id: Some(scan_id.to_owned()),
                attempt_id: Some(scan_id.to_owned()),
                class: if security { "audit" } else { "diagnostic" }.into(),
                severity: "warning".into(),
                phase: "scan".into(),
                code,
                item_id: None,
                sequence_no,
                path_role: Some("source".into()),
                message_key: if security {
                    "security_rejected".into()
                } else {
                    "scan_warning".into()
                },
                payload: serde_json::json!({
                    "cause_chain": diagnostic_cause_chain(warning),
                }),
                contains_sensitive_path: true,
                protected: security,
            },
        );
        Ok(())
    }
    fn record_metric(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        self.record_metric_row(run_id, phase, elapsed_ms, item_count)
    }
}

impl PlanStore for SqliteScanStore {
    fn load_completed_scan(&self, scan_id: &str) -> Result<Vec<ScannedFile>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let completed: Option<String> = conn
            .query_row(
                "SELECT id FROM scan_runs WHERE id=?1 AND status='completed'",
                params![scan_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if completed.is_none() {
            return Err("scan_not_completed".into());
        }
        load_scan_snapshot_items(&conn, scan_id)
    }

    fn load_completed_scan_page(
        &self,
        scan_id: &str,
        after_path: Option<&Path>,
        limit: usize,
    ) -> Result<Vec<ScannedFile>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        ensure_scan_mutable(&conn, scan_id)?;
        let completed: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM scan_runs WHERE id=?1 AND status='completed')",
                params![scan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !completed {
            return Err("scan_not_completed".into());
        }
        let (cursor_encoding, cursor_blob) = after_path
            .map(encode_path)
            .map_or((None, None), |(encoding, blob)| {
                (Some(encoding), Some(blob))
            });
        let limit = i64::try_from(limit.max(1)).map_err(|_| "plan_page_limit_invalid")?;
        let mut statement = conn
            .prepare(
                "SELECT path_encoding,path_blob,size_bytes,mtime_ns,content_sha256,
                        file_identity,fingerprint_version,metadata_json,metadata_status,kind
                   FROM scan_items
                  WHERE scan_id=?1
                    AND (?2 IS NULL OR path_encoding>?2
                         OR (path_encoding=?2 AND path_blob>?3))
                  ORDER BY path_encoding,path_blob
                  LIMIT ?4",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(
                params![scan_id, cursor_encoding, cursor_blob, limit],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .map(
                |(
                    path_encoding,
                    path_blob,
                    size_bytes,
                    mtime_ns,
                    content_sha256,
                    file_identity,
                    fingerprint_version,
                    metadata_json,
                    metadata_status,
                    kind,
                )| {
                    let metadata = match metadata_status.as_str() {
                        "ok" => Some(
                            serde_json::from_str(
                                metadata_json
                                    .as_deref()
                                    .ok_or_else(|| "scan_snapshot_metadata_missing".to_string())?,
                            )
                            .map_err(|error| format!("scan_snapshot_metadata_invalid:{error}"))?,
                        ),
                        "error" | "not_applicable" => None,
                        value => return Err(format!("scan_snapshot_status_unknown:{value}")),
                    };
                    Ok(ScannedFile {
                        id: Uuid::new_v4(),
                        path: decode_path(&path_encoding, &path_blob)?,
                        fingerprint: FileFingerprint {
                            size_bytes: u64::try_from(size_bytes)
                                .map_err(|_| "scan_snapshot_size_invalid".to_string())?,
                            mtime_ns: mtime_ns
                                .parse()
                                .map_err(|_| "scan_snapshot_mtime_invalid".to_string())?,
                            content_sha256,
                            file_identity,
                            version: u16::try_from(fingerprint_version).map_err(|_| {
                                "scan_snapshot_fingerprint_version_invalid".to_string()
                            })?,
                        },
                        metadata,
                        kind: match kind.as_str() {
                            "image" => FileKind::Image,
                            "music" => FileKind::Music,
                            value => return Err(format!("scan_snapshot_kind_unknown:{value}")),
                        },
                    })
                },
            )
            .collect()
    }

    fn completed_scan_snapshot_hash(&self, scan_id: &str) -> Result<String, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        ensure_scan_mutable(&conn, scan_id)?;
        conn.query_row(
            "SELECT snapshot_hash FROM scan_runs
                  WHERE id=?1 AND status='completed'
                    AND snapshot_version=?2 AND snapshot_hash IS NOT NULL",
            params![scan_id, music_folder_core::SCAN_SNAPSHOT_VERSION as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "scan_snapshot_missing_or_legacy".to_string())
    }

    fn begin_plan(
        &self,
        scan_id: &str,
        target_root: &Path,
        naming: &music_folder_core::NamingRules,
    ) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let rules = serde_json::to_string(naming).map_err(|e| e.to_string())?;
        let (target_encoding, target_blob) = encode_path(target_root);
        let scan_snapshot_hash = self.completed_scan_snapshot_hash(scan_id)?;
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        ensure_scan_mutable(&tx, scan_id)?;
        let started_at = now();
        tx.execute("INSERT INTO plan_runs(id,scan_id,target_root,target_root_encoding,target_root_blob,scan_snapshot_hash,rules_json,rules_schema_version,snapshot_version,path_policy_version,status,started_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'running',?11)", params![id,scan_id,target_root.to_string_lossy(),target_encoding,target_blob,scan_snapshot_hash,rules,i64::from(NAMING_RULES_SCHEMA_VERSION),PLAN_SNAPSHOT_VERSION as i64,music_folder_core::WINDOWS_PATH_POLICY_VERSION as i64,started_at]).map_err(|error| error.to_string())?;
        insert_workflow_attempt(
            &tx,
            &id,
            scan_id,
            scan_id,
            "plan",
            Some(scan_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        tx.execute(
            "INSERT INTO plan_builds(id,plan_id,owner_id,status,phase,created_at,updated_at)
             VALUES(?1,?1,?2,'building','drafting',?3,?3)",
            params![id, Uuid::new_v4().to_string(), started_at],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn save_plan_items(&self, plan_id: &str, items: &[PlanItem]) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let (root_encoding, root_blob, rules_json, rules_schema_version): (
            String,
            Vec<u8>,
            String,
            i64,
        ) = tx
            .query_row(
                "SELECT target_root_encoding,target_root_blob,rules_json,rules_schema_version
                   FROM plan_runs WHERE id=?1 AND status='running'",
                params![plan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| error.to_string())?;
        let target_root = decode_path(&root_encoding, &root_blob)?;
        let naming = current_naming_rules(
            rules_schema_version,
            &rules_json,
            "legacy_plan_non_executable",
        )?;
        for item in items {
            validate_plan_item_for_persistence(item, &target_root, naming.allow_long_paths)?;
            let issues_json = serialized_plan_issues(item)?;
            let group_id = item
                .conflict_group_id
                .map(|value| persisted_conflict_group_id(plan_id, value));
            let target = item
                .target
                .as_ref()
                .map(|value| value.to_string_lossy().into_owned());
            let (source_encoding, source_blob) = encode_path(&item.file.path);
            let (target_encoding, target_blob) = item
                .target
                .as_deref()
                .map(encode_path)
                .map_or((None, None), |(encoding, raw)| (Some(encoding), Some(raw)));
            tx.execute("INSERT INTO plan_items(id,plan_id,ordinal,source_path,source_path_encoding,source_path_blob,target_path,target_path_encoding,target_path_blob,conflict_group_id,action,execution_disposition,risk,reason,issues_schema_version,issues_json,source_size_bytes,source_mtime_ns,source_file_identity,source_identity_version,source_content_hash,source_hash_algorithm,source_hash_version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)", params![item.id.to_string(),plan_id,item.ordinal as i64,item.file.path.to_string_lossy(),source_encoding,source_blob,target,target_encoding,target_blob,group_id,plan_action_name(item.action),disposition_name(item.disposition),risk_name(item.risk),item.reason,i64::from(PLAN_ISSUES_SCHEMA_VERSION),issues_json,item.file.fingerprint.size_bytes as i64,item.file.fingerprint.mtime_ns.to_string(),item.file.fingerprint.file_identity.as_deref().map(str::as_bytes),item.file.fingerprint.file_identity.as_ref().map(|_|item.file.fingerprint.version as i64),item.file.fingerprint.content_sha256.as_deref().map(str::as_bytes),item.file.fingerprint.content_sha256.as_ref().map(|_|"sha256"),item.file.fingerprint.content_sha256.as_ref().map(|_|1_i64)]).map_err(|error| error.to_string())?;
            if let (Some(group_id), Some(target)) = (&group_id, &target) {
                tx.execute("INSERT OR IGNORE INTO plan_conflict_groups(id,plan_id,kind,normalized_target_path,target_path) VALUES(?1,?2,'plan_items',?3,?4)", params![group_id,plan_id,windows_path_key(Path::new(target)),target]).map_err(|error| error.to_string())?;
                tx.execute("INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id) VALUES(?1,?2)", params![group_id,item.id.to_string()]).map_err(|error| error.to_string())?;
            } else if let Some(group_id) = &group_id {
                if !item.conflict_candidates.is_empty() {
                    tx.execute("INSERT OR IGNORE INTO plan_conflict_groups(id,plan_id,kind,normalized_target_path,target_path) VALUES(?1,?2,'image_destination','', '')", params![group_id,plan_id]).map_err(|error| error.to_string())?;
                    tx.execute("INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id) VALUES(?1,?2)", params![group_id,item.id.to_string()]).map_err(|error| error.to_string())?;
                    for (index, candidate) in item.conflict_candidates.iter().enumerate() {
                        let ordinal = index as i64 + 1;
                        let (candidate_encoding, candidate_blob) =
                            encode_path(&candidate.target_directory);
                        tx.execute("INSERT INTO plan_conflict_candidates(conflict_group_id,ordinal,target_path,target_path_encoding,target_path_blob) VALUES(?1,?2,?3,?4,?5)", params![group_id,ordinal,candidate.target_directory.to_string_lossy(),candidate_encoding,candidate_blob]).map_err(|error| error.to_string())?;
                        for member in &candidate.music_item_ids {
                            tx.execute("INSERT INTO plan_conflict_candidate_members(conflict_group_id,candidate_ordinal,plan_item_id) VALUES(?1,?2,?3)", params![group_id,ordinal,member.to_string()]).map_err(|error| error.to_string())?;
                        }
                    }
                }
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

    fn finish_plan(
        &self,
        plan_id: &str,
        conflict_count: u64,
        risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String> {
        let staged_count: i64 = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .query_row(
                "SELECT COUNT(*) FROM plan_item_staging WHERE build_id=?1",
                params![plan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if staged_count > 0 {
            return self.complete_plan_build(plan_id, conflict_count, risk_count, snapshot_hash);
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let finished_at = now();
        let changed = tx
            .execute(
                "UPDATE plan_runs SET status='completed',finished_at=?2,
                        conflict_count=?3,risk_count=?4,snapshot_hash=?5
                  WHERE id=?1 AND status='running'",
                params![
                    plan_id,
                    finished_at,
                    i64::try_from(conflict_count).map_err(|_| "plan_conflict_count_invalid")?,
                    i64::try_from(risk_count).map_err(|_| "plan_risk_count_invalid")?,
                    snapshot_hash
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("plan_not_running".into());
        }
        let item_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM plan_items WHERE plan_id=?1",
                params![plan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        finish_workflow_attempt(
            &tx,
            plan_id,
            "plan",
            RunStatus::Completed,
            finished_at,
            u64::try_from(item_count).map_err(|_| "plan_item_count_invalid")?,
            conflict_count,
            risk_count,
            None,
        )?;
        tx.execute(
            "UPDATE plan_builds SET status='completed',phase='completed',updated_at=?2
              WHERE id=?1 AND status='building'",
            params![plan_id, now()],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }
    fn fail_plan(&self, plan_id: &str) -> Result<(), String> {
        self.abort_plan_build(plan_id, RunStatus::Failed)
    }
    fn record_metric(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        self.record_metric_row(run_id, phase, elapsed_ms, item_count)
    }
}

impl PlanBuildStore for SqliteScanStore {
    fn stage_plan_batch(
        &self,
        build_id: &str,
        items: &[StagedPlanItem],
        anchors: &[PlanMusicAnchor],
    ) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let (target_root, naming) = plan_build_persistence_context(&tx, build_id)?;
        let building: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_builds WHERE id=?1 AND status='building')",
                params![build_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !building {
            return Err("plan_build_not_active".into());
        }
        for staged in items {
            validate_plan_item_for_persistence(
                &staged.item,
                &target_root,
                naming.allow_long_paths,
            )?;
            let (draft_json, source_encoding, source_blob, target_encoding, target_blob) =
                encode_staged_plan_item(staged)?;
            let target_key = staging_target_key(&staged.item);
            tx.execute(
                "INSERT INTO plan_item_staging(
                     build_id,ordinal,source_path_encoding,source_path_blob,draft_json,
                     normalized_target_key,target_path_encoding,target_path_blob,
                     original_target_key,parent_plan_item_id,change_origin,change_reason
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?6,?9,?10,?11)",
                params![
                    build_id,
                    i64::try_from(staged.item.ordinal).map_err(|_| "plan_ordinal_invalid")?,
                    source_encoding,
                    source_blob,
                    draft_json,
                    target_key,
                    target_encoding,
                    target_blob,
                    staged.parent_plan_item_id,
                    staged.change_origin,
                    staged.change_reason,
                ],
            )
            .map_err(|error| error.to_string())?;
        }
        for anchor in anchors {
            let (source_encoding, source_blob) = encode_path(&anchor.source_directory);
            let (target_encoding, target_blob) = encode_path(&anchor.target_directory);
            let (disc_encoding, disc_blob) = anchor
                .disc_parent
                .as_deref()
                .map(encode_path)
                .map_or((None, None), |(encoding, blob)| {
                    (Some(encoding), Some(blob))
                });
            tx.execute(
                "INSERT OR IGNORE INTO plan_music_anchor_staging(
                     build_id,source_directory_encoding,source_directory_blob,
                     target_directory_encoding,target_directory_blob,
                     disc_parent_encoding,disc_parent_blob,music_item_id
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    build_id,
                    source_encoding,
                    source_blob,
                    target_encoding,
                    target_blob,
                    disc_encoding,
                    disc_blob,
                    anchor.music_item_id.to_string(),
                ],
            )
            .map_err(|error| error.to_string())?;
        }
        let cursor = items.last().map_or(0_i64, |item| {
            i64::try_from(item.item.ordinal).unwrap_or(i64::MAX)
        });
        tx.execute(
            "UPDATE plan_builds
                SET updated_at=?2,cursor_ordinal=MAX(cursor_ordinal,?3),
                    peak_batch_items=MAX(peak_batch_items,?4),phase='drafting'
              WHERE id=?1 AND status='building'",
            params![
                build_id,
                now(),
                cursor,
                i64::try_from(items.len()).map_err(|_| "plan_batch_size_invalid")?
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    fn load_staged_plan_page(
        &self,
        build_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<StagedPlanItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        load_staged_plan_page_from_connection(&conn, build_id, after_ordinal, limit)
    }

    fn update_staged_plan_batch(
        &self,
        build_id: &str,
        items: &[StagedPlanItem],
        update_original_target: bool,
        allocated_target_keys: &[String],
    ) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let (target_root, naming) = plan_build_persistence_context(&tx, build_id)?;
        for staged in items {
            validate_plan_item_for_persistence(
                &staged.item,
                &target_root,
                naming.allow_long_paths,
            )?;
            let (draft_json, source_encoding, source_blob, target_encoding, target_blob) =
                encode_staged_plan_item(staged)?;
            let target_key = staging_target_key(&staged.item);
            let changed = tx
                .execute(
                    "UPDATE plan_item_staging
                        SET draft_json=?3,source_path_encoding=?4,source_path_blob=?5,
                            target_path_encoding=?6,target_path_blob=?7,
                            normalized_target_key=?8,
                            original_target_key=CASE WHEN ?9=1 THEN ?8
                                                     ELSE original_target_key END
                      WHERE build_id=?1 AND ordinal=?2
                        AND EXISTS(SELECT 1 FROM plan_builds
                                    WHERE id=?1 AND status='building')",
                    params![
                        build_id,
                        i64::try_from(staged.item.ordinal).map_err(|_| "plan_ordinal_invalid")?,
                        draft_json,
                        source_encoding,
                        source_blob,
                        target_encoding,
                        target_blob,
                        target_key,
                        i64::from(update_original_target),
                    ],
                )
                .map_err(|error| error.to_string())?;
            if changed != 1 {
                return Err("plan_staging_item_not_found".into());
            }
        }
        for key in allocated_target_keys {
            tx.execute(
                "INSERT OR IGNORE INTO plan_target_allocations(build_id,normalized_target_key)
                 VALUES(?1,?2)",
                params![build_id, key],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.execute(
            "UPDATE plan_builds
                SET updated_at=?2,phase='resolving',
                    peak_batch_items=MAX(peak_batch_items,?3)
              WHERE id=?1 AND status='building'",
            params![
                build_id,
                now(),
                i64::try_from(items.len()).map_err(|_| "plan_batch_size_invalid")?
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    fn load_nearest_music_anchors(
        &self,
        build_id: &str,
        source_ancestors: &[PathBuf],
    ) -> Result<Vec<PlanMusicAnchor>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        for directory in source_ancestors {
            let (source_encoding, source_blob) = encode_path(directory);
            let mut statement = conn
                .prepare(
                    "SELECT target_directory_encoding,target_directory_blob,
                            disc_parent_encoding,disc_parent_blob,music_item_id
                       FROM plan_music_anchor_staging
                      WHERE build_id=?1 AND source_directory_encoding=?2
                        AND source_directory_blob=?3
                      ORDER BY target_directory_encoding,target_directory_blob,music_item_id",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(params![build_id, source_encoding, source_blob], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            if rows.is_empty() {
                continue;
            }
            return rows
                .into_iter()
                .map(
                    |(target_encoding, target_blob, disc_encoding, disc_blob, item_id)| {
                        let disc_parent = match (disc_encoding, disc_blob) {
                            (Some(encoding), Some(blob)) => Some(decode_path(&encoding, &blob)?),
                            (None, None) => None,
                            _ => return Err("plan_anchor_disc_encoding_incomplete".into()),
                        };
                        Ok(PlanMusicAnchor {
                            source_directory: directory.clone(),
                            target_directory: decode_path(&target_encoding, &target_blob)?,
                            disc_parent,
                            music_item_id: Uuid::parse_str(&item_id)
                                .map_err(|_| "plan_anchor_item_id_invalid")?,
                        })
                    },
                )
                .collect();
        }
        Ok(Vec::new())
    }

    fn original_target_rank(
        &self,
        build_id: &str,
        normalized_target_key: &str,
        ordinal: u64,
    ) -> Result<u64, String> {
        let count: i64 = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .query_row(
                "SELECT COUNT(*) FROM plan_item_staging
                  WHERE build_id=?1 AND original_target_key=?2 AND ordinal<=?3",
                params![
                    build_id,
                    normalized_target_key,
                    i64::try_from(ordinal).map_err(|_| "plan_ordinal_invalid")?
                ],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        u64::try_from(count).map_err(|_| "plan_target_rank_invalid".into())
    }

    fn original_target_exists_after(
        &self,
        build_id: &str,
        normalized_target_key: &str,
        ordinal: u64,
    ) -> Result<bool, String> {
        self.connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_item_staging
                  WHERE build_id=?1 AND original_target_key=?2 AND ordinal>?3)",
                params![
                    build_id,
                    normalized_target_key,
                    i64::try_from(ordinal).map_err(|_| "plan_ordinal_invalid")?
                ],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    fn target_is_allocated(
        &self,
        build_id: &str,
        normalized_target_key: &str,
    ) -> Result<bool, String> {
        self.connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_target_allocations
                  WHERE build_id=?1 AND normalized_target_key=?2)",
                params![build_id, normalized_target_key],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    fn staged_target_count(
        &self,
        build_id: &str,
        normalized_target_key: &str,
    ) -> Result<u64, String> {
        let count: i64 = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .query_row(
                "SELECT COUNT(*) FROM plan_item_staging
                  WHERE build_id=?1 AND normalized_target_key=?2",
                params![build_id, normalized_target_key],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        u64::try_from(count).map_err(|_| "plan_target_count_invalid".into())
    }

    fn staged_plan_item_count(&self, build_id: &str) -> Result<u64, String> {
        let count: i64 = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .query_row(
                "SELECT COUNT(*) FROM plan_item_staging WHERE build_id=?1",
                params![build_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        u64::try_from(count).map_err(|_| "plan_staging_count_invalid".into())
    }

    fn publish_staged_plan_page(
        &self,
        build_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<PlanItem>, String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let page = load_staged_plan_page_from_connection(&conn, build_id, after_ordinal, limit)?;
        if page.is_empty() {
            return Ok(Vec::new());
        }
        let (target_root, naming) = plan_build_persistence_context(&conn, build_id)?;
        for staged in &page {
            validate_plan_item_for_persistence(
                &staged.item,
                &target_root,
                naming.allow_long_paths,
            )?;
        }
        let plan_id: String = conn
            .query_row(
                "SELECT plan_id FROM plan_builds WHERE id=?1 AND status='building'",
                params![build_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan_build_not_active".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        for staged in &page {
            insert_published_plan_item(&tx, &plan_id, staged)?;
        }
        tx.execute(
            "UPDATE plan_builds SET phase='publishing',updated_at=?2 WHERE id=?1",
            params![build_id, now()],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(page.into_iter().map(|staged| staged.item).collect())
    }

    fn complete_plan_build(
        &self,
        build_id: &str,
        conflict_count: u64,
        risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let plan_id: String = tx
            .query_row(
                "SELECT plan_id FROM plan_builds WHERE id=?1 AND status='building'",
                params![build_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan_build_not_active".to_string())?;
        let (staged_count, published_count): (i64, i64) = tx
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM plan_item_staging WHERE build_id=?1),
                    (SELECT COUNT(*) FROM plan_items WHERE plan_id=?2)",
                params![build_id, plan_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        if staged_count != published_count {
            return Err(format!(
                "plan_publish_count_mismatch:{staged_count}:{published_count}"
            ));
        }
        let finished_at = now();
        let changed = tx
            .execute(
                "UPDATE plan_runs
                    SET status='completed',finished_at=?2,conflict_count=?3,
                        risk_count=?4,snapshot_hash=?5
                  WHERE id=?1 AND status='running'",
                params![
                    plan_id,
                    finished_at,
                    i64::try_from(conflict_count).map_err(|_| "plan_conflict_count_invalid")?,
                    i64::try_from(risk_count).map_err(|_| "plan_risk_count_invalid")?,
                    snapshot_hash
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("plan_not_running".into());
        }
        finish_workflow_attempt(
            &tx,
            &plan_id,
            "plan",
            RunStatus::Completed,
            finished_at,
            u64::try_from(published_count).map_err(|_| "plan_item_count_invalid")?,
            conflict_count,
            risk_count,
            None,
        )?;
        tx.execute(
            "DELETE FROM plan_target_allocations WHERE build_id=?1",
            params![build_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_music_anchor_staging WHERE build_id=?1",
            params![build_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_item_staging WHERE build_id=?1",
            params![build_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE plan_builds
                SET status='completed',phase='completed',updated_at=?2
              WHERE id=?1 AND status='building'",
            params![build_id, now()],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    fn abort_plan_build(&self, build_id: &str, status: RunStatus) -> Result<(), String> {
        let status_name = match status {
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            _ => return Err("plan_build_abort_status_invalid".into()),
        };
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let plan_id: Option<String> = tx
            .query_row(
                "SELECT plan_id FROM plan_builds WHERE id=?1",
                params![build_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some(plan_id) = plan_id else {
            return Ok(());
        };
        tx.execute(
            "DELETE FROM plan_revision_changes WHERE child_plan_id=?1",
            params![plan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_conflict_groups WHERE plan_id=?1",
            params![plan_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute("DELETE FROM plan_items WHERE plan_id=?1", params![plan_id])
            .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_target_allocations WHERE build_id=?1",
            params![build_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_music_anchor_staging WHERE build_id=?1",
            params![build_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM plan_item_staging WHERE build_id=?1",
            params![build_id],
        )
        .map_err(|error| error.to_string())?;
        let finished_at = now();
        let changed = tx
            .execute(
                "UPDATE plan_runs SET status=?2,finished_at=?3
              WHERE id=?1 AND status='running'",
                params![plan_id, status_name, finished_at],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("plan_not_running".into());
        }
        let attempt_kind: String = tx
            .query_row(
                "SELECT kind FROM workflow_attempts WHERE id=?1",
                params![plan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        finish_workflow_attempt(
            &tx,
            &plan_id,
            &attempt_kind,
            status,
            finished_at,
            0,
            0,
            1,
            None,
        )?;
        tx.execute(
            "UPDATE plan_builds SET status=?2,phase='aborted',updated_at=?3
              WHERE id=?1 AND status='building'",
            params![build_id, status_name, finished_at],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }
}

impl PlanRevisionStore for SqliteScanStore {
    fn begin_plan_revision(&self, parent_plan_id: &str) -> Result<PlanRevisionBuild, String> {
        type ParentContext = (
            String,
            String,
            Vec<u8>,
            String,
            String,
            i64,
            String,
            i64,
            i64,
            String,
        );
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        ensure_plan_mutable(&conn, parent_plan_id)?;
        let parent: ParentContext = conn
            .query_row(
                "SELECT scan_id,target_root_encoding,target_root_blob,target_root,rules_json,
                        rules_schema_version,snapshot_hash,snapshot_version,
                        path_policy_version,scan_snapshot_hash
                   FROM plan_runs WHERE id=?1 AND status='completed'",
                params![parent_plan_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "parent_plan_not_completed".to_string())?;
        if parent.7 != i64::from(PLAN_SNAPSHOT_VERSION)
            || parent.8 != i64::from(music_folder_core::WINDOWS_PATH_POLICY_VERSION)
        {
            return Err("legacy_plan_non_revisable".into());
        }
        if persisted_plan_snapshot_hash(&conn, parent_plan_id)? != parent.6 {
            return Err("parent_plan_snapshot_mismatch".into());
        }
        let target_root = decode_path(&parent.1, &parent.2)?;
        let naming = current_naming_rules(parent.5, &parent.4, "legacy_plan_non_revisable")?;
        let child_plan_id = Uuid::new_v4().to_string();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        ensure_plan_mutable(&tx, parent_plan_id)?;
        let started_at = now();
        tx.execute(
            "INSERT INTO plan_runs(
                 id,scan_id,parent_plan_id,target_root,target_root_encoding,target_root_blob,
                 scan_snapshot_hash,rules_json,rules_schema_version,snapshot_version,
                 path_policy_version,status,started_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'running',?12)",
            params![
                child_plan_id,
                parent.0,
                parent_plan_id,
                parent.3,
                parent.1,
                parent.2,
                parent.9,
                parent.4,
                parent.5,
                parent.7,
                parent.8,
                started_at,
            ],
        )
        .map_err(|error| error.to_string())?;
        insert_workflow_attempt(
            &tx,
            &child_plan_id,
            &parent.0,
            parent_plan_id,
            "plan_revision",
            Some(parent_plan_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        tx.execute(
            "INSERT INTO plan_builds(
                 id,plan_id,parent_plan_id,owner_id,status,phase,created_at,updated_at
             ) VALUES(?1,?1,?2,?3,'building','drafting',?4,?4)",
            params![
                child_plan_id,
                parent_plan_id,
                Uuid::new_v4().to_string(),
                started_at
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(PlanRevisionBuild {
            child_plan_id: child_plan_id.clone(),
            build_id: child_plan_id,
            scan_id: parent.0,
            scan_snapshot_hash: parent.9,
            target_root,
            naming,
        })
    }

    fn load_parent_plan_page(
        &self,
        parent_plan_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<StagedPlanItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        ensure_plan_mutable(&conn, parent_plan_id)?;
        let completed: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_runs WHERE id=?1 AND status='completed')",
                params![parent_plan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !completed {
            return Err("parent_plan_not_completed".into());
        }
        let (root_encoding, root_blob, rules_json, rules_schema_version): (
            String,
            Vec<u8>,
            String,
            i64,
        ) = conn
            .query_row(
                "SELECT target_root_encoding,target_root_blob,rules_json,rules_schema_version
                   FROM plan_runs WHERE id=?1 AND status='completed'",
                params![parent_plan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| error.to_string())?;
        let target_root = decode_path(&root_encoding, &root_blob)?;
        let naming = current_naming_rules(
            rules_schema_version,
            &rules_json,
            "legacy_plan_non_revisable",
        )?;
        let page = load_persisted_plan_page(&conn, parent_plan_id, after_ordinal, limit)?;
        for staged in &page {
            validate_plan_item_for_persistence(
                &staged.item,
                &target_root,
                naming.allow_long_paths,
            )?;
        }
        Ok(page)
    }

    fn revise_plan(
        &self,
        parent_plan_id: &str,
        changes: &[music_folder_core::ports::ManualTargetChange],
    ) -> Result<String, String> {
        music_folder_core::usecases::execute_bounded_plan_revision(
            self,
            parent_plan_id,
            changes,
            512,
        )
        .map_err(|error| error.to_string())
    }
}

fn plan_action_name(action: PlanAction) -> &'static str {
    match action {
        PlanAction::Move => "move",
        PlanAction::Skip => "skip",
    }
}
fn risk_name(risk: Risk) -> &'static str {
    match risk {
        Risk::None => "none",
        Risk::InvalidTarget => "invalid_target",
        Risk::PathTooLong => "path_too_long",
        Risk::Conflict => "conflict",
        Risk::MetadataMissing => "metadata_missing",
    }
}

fn persisted_plan_snapshot_hash(connection: &Connection, plan_id: &str) -> Result<String, String> {
    type PlanSnapshotContextRow = (String, String, String, Vec<u8>, String, i64, i64, i64);
    let context: Option<PlanSnapshotContextRow> = connection
        .query_row(
            "SELECT scan_id,scan_snapshot_hash,target_root_encoding,target_root_blob,
                    rules_json,rules_schema_version,snapshot_version,path_policy_version
               FROM plan_runs WHERE id=?1",
            params![plan_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let (
        scan_id,
        scan_snapshot_hash,
        target_root_encoding,
        target_root_blob,
        rules_json,
        rules_schema_version,
        snapshot_version,
        path_policy_version,
    ) = context.ok_or_else(|| "plan_not_found".to_string())?;
    if snapshot_version != i64::from(PLAN_SNAPSHOT_VERSION)
        || path_policy_version != i64::from(music_folder_core::WINDOWS_PATH_POLICY_VERSION)
    {
        return Err("legacy_plan_snapshot_encoding".into());
    }
    let target_root = decode_path(&target_root_encoding, &target_root_blob)?;
    let naming = current_naming_rules(
        rules_schema_version,
        &rules_json,
        "legacy_plan_snapshot_encoding",
    )?;
    let rules_schema_version = u32::try_from(rules_schema_version)
        .map_err(|_| "legacy_plan_snapshot_encoding".to_owned())?;
    let item_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM plan_items WHERE plan_id=?1",
            params![plan_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let item_count = u64::try_from(item_count).map_err(|_| "plan_snapshot_item_count_invalid")?;
    let mut hasher = music_folder_core::PlanAuthorizationHasher::new_with_rules_schema_version(
        &scan_id,
        &scan_snapshot_hash,
        &target_root,
        rules_schema_version,
        &naming,
        item_count,
    );
    let mut after_ordinal = 0_u64;
    loop {
        let page = load_persisted_plan_page(connection, plan_id, after_ordinal, 512)?;
        if page.is_empty() {
            break;
        }
        for staged in page {
            validate_plan_item_for_persistence(
                &staged.item,
                &target_root,
                naming.allow_long_paths,
            )?;
            if staged.item.ordinal <= after_ordinal {
                return Err("plan_snapshot_items_not_strictly_ordered".into());
            }
            after_ordinal = staged.item.ordinal;
            hasher.update(&staged.item);
        }
    }
    hasher.finish()
}

impl ApplyStore for SqliteScanStore {
    fn load_completed_plan(&self, plan_id: &str) -> Result<Vec<ApplyItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        ensure_plan_mutable(&conn, plan_id)?;
        let plan: Option<(String, Vec<u8>, String, i64, i64, i64)> = conn
            .query_row(
                "SELECT target_root_encoding,target_root_blob,rules_json,rules_schema_version,
                        snapshot_version,path_policy_version
                   FROM plan_runs WHERE id=?1 AND status='completed'",
                params![plan_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (
            target_root_encoding,
            target_root_blob,
            rules_json,
            rules_schema_version,
            snapshot_version,
            path_policy_version,
        ) = plan.ok_or_else(|| "plan_not_completed".to_string())?;
        if snapshot_version != i64::from(PLAN_SNAPSHOT_VERSION)
            || path_policy_version != i64::from(music_folder_core::WINDOWS_PATH_POLICY_VERSION)
        {
            return Err("legacy_plan_non_executable".into());
        }
        let target_root = decode_path(&target_root_encoding, &target_root_blob)?;
        let naming = current_naming_rules(
            rules_schema_version,
            &rules_json,
            "legacy_plan_non_executable",
        )?;
        let mut statement = conn.prepare("SELECT id,ordinal,source_path_encoding,source_path_blob,target_path_encoding,target_path_blob,action,execution_disposition,risk,reason,issues_schema_version,issues_json,source_size_bytes,source_mtime_ns,source_file_identity,source_identity_version,source_content_hash,source_hash_version FROM plan_items WHERE plan_id=?1 ORDER BY ordinal").map_err(|e| e.to_string())?;
        let items = statement
            .query_map(params![plan_id], |r| {
                let source_encoding = r.get::<_, String>(2)?;
                let source_blob = r.get::<_, Vec<u8>>(3)?;
                let target_encoding = r.get::<_, Option<String>>(4)?;
                let target_blob = r.get::<_, Option<Vec<u8>>>(5)?;
                let source_size = r.get::<_, Option<i64>>(12)?;
                let source_mtime = r.get::<_, Option<String>>(13)?;
                let source_identity = r.get::<_, Option<Vec<u8>>>(14)?;
                let identity_version = r.get::<_, Option<i64>>(15)?;
                let source_hash = r.get::<_, Option<Vec<u8>>>(16)?;
                let hash_version = r.get::<_, Option<i64>>(17)?;
                let action = if r.get::<_, String>(6)? == "move" {
                    PlanAction::Move
                } else {
                    PlanAction::Skip
                };
                let disposition = match r.get::<_, String>(7)?.as_str() {
                    "executable" => ExecutionDisposition::Executable,
                    "skip" => ExecutionDisposition::Skip,
                    "blocked" => ExecutionDisposition::Blocked,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                let risk = match r.get::<_, String>(8)?.as_str() {
                    "none" => Risk::None,
                    "conflict" => Risk::Conflict,
                    "metadata_missing" => Risk::MetadataMissing,
                    "path_too_long" => Risk::PathTooLong,
                    _ => Risk::InvalidTarget,
                };
                let reason = r.get::<_, Option<String>>(9)?;
                if r.get::<_, i64>(10)? != i64::from(PLAN_ISSUES_SCHEMA_VERSION) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let issues = serde_json::from_str::<Vec<PlanIssue>>(&r.get::<_, String>(11)?)
                    .map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(std::io::Error::other(error).into())
                    })?;
                Ok(ApplyItem {
                    plan_item_id: r.get(0)?,
                    ordinal: r.get::<_, i64>(1)? as u64,
                    source: decode_path(&source_encoding, &source_blob).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(std::io::Error::other(error).into())
                    })?,
                    target: match (target_encoding, target_blob) {
                        (Some(encoding), Some(raw)) => {
                            Some(decode_path(&encoding, &raw).map_err(|error| {
                                rusqlite::Error::ToSqlConversionFailure(
                                    std::io::Error::other(error).into(),
                                )
                            })?)
                        }
                        (None, None) => None,
                        _ => {
                            return Err(rusqlite::Error::InvalidQuery);
                        }
                    },
                    action,
                    disposition,
                    risk,
                    reason,
                    issues,
                    source_fingerprint: match (source_size, source_mtime) {
                        (Some(size), Some(mtime)) => Some(FileFingerprint {
                            size_bytes: u64::try_from(size)
                                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(12, size))?,
                            mtime_ns: mtime.parse().map_err(|_| {
                                rusqlite::Error::InvalidColumnType(
                                    13,
                                    "source_mtime_ns".into(),
                                    rusqlite::types::Type::Text,
                                )
                            })?,
                            content_sha256: source_hash
                                .map(String::from_utf8)
                                .transpose()
                                .map_err(|error| {
                                    rusqlite::Error::ToSqlConversionFailure(
                                        std::io::Error::other(error).into(),
                                    )
                                })?,
                            file_identity: source_identity
                                .map(String::from_utf8)
                                .transpose()
                                .map_err(|error| {
                                    rusqlite::Error::ToSqlConversionFailure(
                                        std::io::Error::other(error).into(),
                                    )
                                })?,
                            version: u16::try_from(hash_version.or(identity_version).unwrap_or(1))
                                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(17, 0))?,
                        }),
                        (None, None) => None,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                    target_root: Some(target_root.clone()),
                    allow_long_paths: naming.allow_long_paths,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for item in &items {
            validate_apply_item_target(item)?;
        }
        Ok(items)
    }
    fn validate_plan_snapshot(&self, plan_id: &str) -> Result<(), String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        ensure_plan_mutable(&conn, plan_id)?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT snapshot_hash FROM plan_runs WHERE id=?1 AND status='completed'",
                params![plan_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        let Some(stored) = stored else {
            return Err("plan_snapshot_missing".into());
        };
        if persisted_plan_snapshot_hash(&conn, plan_id)? != stored {
            diagnostics::insert_diagnostic_best_effort(
                &conn,
                DiagnosticEventInput {
                    run_id: Some(plan_id.to_owned()),
                    attempt_id: None,
                    class: "audit".into(),
                    severity: "error".into(),
                    phase: "authorization".into(),
                    code: "plan_snapshot_mismatch".into(),
                    item_id: None,
                    sequence_no: None,
                    path_role: None,
                    message_key: "security_rejected".into(),
                    payload: serde_json::json!({
                        "cause_chain": [{"code": "plan_snapshot_mismatch"}],
                    }),
                    contains_sensitive_path: false,
                    protected: true,
                },
            );
            return Err("plan_snapshot_mismatch".into());
        }
        Ok(())
    }
    fn successful_plan_item_ids(&self, plan_id: &str) -> Result<Vec<String>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT operation.plan_item_id
               FROM operation_logs operation
               JOIN execution_runs execution ON execution.id=operation.execution_id
              WHERE execution.plan_id=?1 AND execution.mode='apply'
                AND operation.result='success' AND operation.source_deleted=1
              UNION
             SELECT DISTINCT journal.plan_item_id
               FROM operation_journal journal
               JOIN execution_runs execution ON execution.id=journal.attempt_id
              WHERE execution.plan_id=?1 AND execution.mode='apply'
                AND journal.direction='apply' AND journal.state='completed'",
            )
            .map_err(|e| e.to_string())?;
        let ids = stmt
            .query_map(params![plan_id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(ids)
    }
    fn begin_execution(&self, plan_id: &str, dry_run: bool) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        ensure_plan_mutable(&transaction, plan_id)?;
        let root_scan_id: String = transaction
            .query_row(
                "SELECT scan_id FROM plan_runs WHERE id=?1",
                params![plan_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let started_at = now();
        let mode = if dry_run { "dry_run" } else { "apply" };
        transaction.execute("INSERT INTO execution_runs(id,plan_id,mode,status,started_at) VALUES(?1,?2,?3,'running',?4)",params![id,plan_id,mode,started_at]).map_err(|e|e.to_string())?;
        insert_workflow_attempt(
            &transaction,
            &id,
            &root_scan_id,
            plan_id,
            mode,
            Some(plan_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }
    fn acquire_apply_lease(&self, plan_id: &str, owner_id: &str) -> Result<MutationLease, String> {
        {
            let conn = self
                .connection
                .lock()
                .map_err(|_| "database mutex poisoned".to_string())?;
            ensure_plan_mutable(&conn, plan_id)?;
        }
        let roots = self.mutation_roots_for_plan(plan_id)?;
        let lease = self.acquire_mutation_lease(&roots, owner_id, None)?;
        let guard = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())
            .and_then(|conn| ensure_plan_mutable(&conn, plan_id));
        if let Err(error) = guard {
            let _ = self.release_mutation_lease(&lease);
            return Err(error);
        }
        Ok(lease)
    }
    fn heartbeat_apply_lease(&self, lease: &MutationLease) -> Result<(), String> {
        self.heartbeat_mutation_lease(lease)
    }
    fn release_apply_lease(&self, lease: &MutationLease) -> Result<(), String> {
        self.release_mutation_lease(lease)
    }
    fn prepare_operation(&self, intent: &OperationIntent) -> Result<(), String> {
        if intent.direction != MutationDirection::Apply {
            return Err("journal_direction_not_apply".into());
        }
        self.prepare_journal_operation(intent)
    }
    fn transition_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.transition_journal_operation(operation_id, from, to, lease, staged, error)
    }
    fn save_operation(&self, execution_id: &str, op: &OperationLog) -> Result<(), String> {
        let (source_encoding, source_blob) = encode_path(&op.source);
        let (target_encoding, target_blob) = op
            .target
            .as_deref()
            .map(encode_path)
            .map_or((None, None), |(encoding, blob)| {
                (Some(encoding), Some(blob))
            });
        let operation_log_id = Uuid::new_v4().to_string();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        connection.execute("INSERT INTO operation_logs(id,execution_id,plan_item_id,sequence_no,source_path,source_path_encoding,source_path_blob,target_path,target_path_encoding,target_path_blob,action,result,error,source_deleted,expected_size,expected_content_sha256,expected_file_identity,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",params![operation_log_id,execution_id,op.plan_item_id,op.sequence_no as i64,op.source.to_string_lossy(),source_encoding,source_blob,op.target.as_ref().map(|p|p.to_string_lossy().into_owned()),target_encoding,target_blob,op.action.as_str(),op.result.as_str(),op.error,op.source_deleted as i32,op.expected_size.map(|size|size as i64),op.expected_content_sha256,op.expected_file_identity,now()]).map_err(|e|e.to_string())?;
        if op.result == OperationResult::Failed {
            let raw_error = op.error.as_deref().unwrap_or("operation_failed");
            let code = stable_diagnostic_code(raw_error, "operation_failed");
            let journal_state: Option<String> = connection
                .query_row(
                    "SELECT state FROM operation_journal
                      WHERE attempt_id=?1 AND plan_item_id=?2
                      ORDER BY transition_no DESC,id DESC LIMIT 1",
                    params![execution_id, op.plan_item_id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);
            let recovery = journal_state.as_deref() == Some("recovery_required")
                || raw_error.starts_with("published_")
                || raw_error.starts_with("source_delete_failed")
                || raw_error.starts_with("conditional_delete_")
                || raw_error.starts_with("journal_transition_");
            let security = security_rejection_code(&code);
            let logical_run_id: Option<String> = connection
                .query_row(
                    "SELECT logical_run_id FROM workflow_attempts WHERE id=?1",
                    params![execution_id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);
            diagnostics::insert_diagnostic_best_effort(
                &connection,
                DiagnosticEventInput {
                    run_id: logical_run_id.or_else(|| Some(execution_id.to_owned())),
                    attempt_id: Some(execution_id.to_owned()),
                    class: if recovery {
                        "recovery"
                    } else if security {
                        "audit"
                    } else {
                        "diagnostic"
                    }
                    .into(),
                    severity: "error".into(),
                    phase: "apply".into(),
                    code,
                    item_id: Some(op.plan_item_id.clone()),
                    sequence_no: Some(op.sequence_no),
                    path_role: Some("target".into()),
                    message_key: if recovery {
                        "workflow_recovery_required"
                    } else if security {
                        "security_rejected"
                    } else {
                        "operation_failed"
                    }
                    .into(),
                    payload: serde_json::json!({
                        "operation_log_id": operation_log_id,
                        "action": op.action.as_str(),
                        "result": op.result.as_str(),
                        "journal_state": journal_state,
                        "cause_chain": diagnostic_cause_chain(raw_error),
                    }),
                    contains_sensitive_path: false,
                    protected: recovery || security,
                },
            );
        }
        Ok(())
    }
    fn finish_execution(
        &self,
        execution_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let finished_at = now();
        reconcile_terminal_journal_summaries(&transaction, finished_at)?;
        let authoritative = terminal_apply_journal_outcome(&transaction, execution_id)?;
        let (effective_status, success, skipped, failed) = if matches!(
            status,
            RunStatus::Completed | RunStatus::Partial | RunStatus::RecoveryRequired
        ) {
            authoritative.map_or((status, success, skipped, failed), |outcome| {
                (
                    outcome.status,
                    outcome.success,
                    outcome.skipped,
                    outcome.failed,
                )
            })
        } else {
            (status, success, skipped, failed)
        };
        let changed = transaction.execute("UPDATE execution_runs SET status=?2,finished_at=?3,success_count=?4,skipped_count=?5,failed_count=?6 WHERE id=?1 AND status='running'",params![execution_id,effective_status.as_str(),finished_at,success as i64,skipped as i64,failed as i64]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("execution_not_running".into());
        }
        let kind: String = transaction
            .query_row(
                "SELECT kind FROM workflow_attempts WHERE id=?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        finish_workflow_attempt(
            &transaction,
            execution_id,
            &kind,
            effective_status,
            finished_at,
            success,
            skipped,
            failed,
            None,
        )?;
        transaction.commit().map_err(|error| error.to_string())
    }
    fn record_metric(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        self.record_metric_row(run_id, phase, elapsed_ms, item_count)
    }
}

impl VerifyStore for SqliteScanStore {
    fn begin_verify(&self, execution_id: &str) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        ensure_execution_mutable(&transaction, execution_id)?;
        let root_scan_id: String = transaction
            .query_row(
                "SELECT plan.scan_id FROM execution_runs execution
                  JOIN plan_runs plan ON plan.id=execution.plan_id
                 WHERE execution.id=?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let started_at = now();
        transaction.execute("INSERT INTO verify_runs(id,execution_id,subject_kind,subject_id,status,started_at) VALUES(?1,?2,'execution',?2,'running',?3)", params![id,execution_id,started_at]).map_err(|e|e.to_string())?;
        insert_workflow_attempt(
            &transaction,
            &id,
            &root_scan_id,
            &format!("execution:{execution_id}"),
            "verify",
            Some(execution_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }
    fn load_successful_operations(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT operation.id,journal.sequence_no,
                        journal.source_path_encoding,journal.source_path_blob,
                        journal.target_path_encoding,journal.target_path_blob,
                        CASE journal.strategy WHEN 'atomic_no_replace_rename' THEN 'move'
                             ELSE 'copy_delete' END,
                        journal.expected_size,journal.expected_content_sha256,
                        CASE journal.strategy WHEN 'copy_publish_delete'
                             THEN COALESCE(journal.staged_file_identity,journal.expected_file_identity)
                             ELSE journal.expected_file_identity END
                   FROM operation_journal journal
                   JOIN operation_logs operation
                     ON operation.execution_id=journal.attempt_id
                    AND operation.sequence_no=journal.sequence_no
                  WHERE journal.attempt_id=?1 AND journal.direction='apply'
                    AND journal.state='completed'
                  UNION ALL
                 SELECT operation.id,operation.sequence_no,
                        operation.source_path_encoding,operation.source_path_blob,
                        operation.target_path_encoding,operation.target_path_blob,
                        operation.action,operation.expected_size,
                        operation.expected_content_sha256,operation.expected_file_identity
                   FROM operation_logs operation
                  WHERE operation.execution_id=?1 AND operation.result='success'
                    AND operation.action IN ('move','copy_delete')
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=operation.execution_id
                           AND journal.direction='apply'
                           AND journal.sequence_no=operation.sequence_no
                           AND journal.state='completed'
                    )
                  ORDER BY sequence_no",
            )
            .map_err(|e| e.to_string())?;
        let items = stmt
            .query_map(params![execution_id], verify_item_from_row)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(items)
    }
    fn save_verify_result(
        &self,
        verify_id: &str,
        operation_id: &str,
        result: OperationResult,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO verify_logs(id,execution_id,verify_run_id,operation_id,result,error,created_at) SELECT ?1,execution_id,id,?3,?4,?5,?6 FROM verify_runs WHERE id=?2", params![Uuid::new_v4().to_string(),verify_id,operation_id,result.as_str(),error,now()]).map_err(|e|e.to_string())?;
        Ok(())
    }
    fn finish_verify(
        &self,
        verify_id: &str,
        status: RunStatus,
        success: u64,
        failed: u64,
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let finished_at = now();
        let changed = transaction.execute("UPDATE verify_runs SET status=?2,finished_at=?3,success_count=?4,failed_count=?5 WHERE id=?1 AND status='running'", params![verify_id,status.as_str(),finished_at,success as i64,failed as i64]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("verify_not_running".into());
        }
        finish_workflow_attempt(
            &transaction,
            verify_id,
            "verify",
            status,
            finished_at,
            success,
            0,
            failed,
            None,
        )?;
        transaction.commit().map_err(|error| error.to_string())
    }
    fn record_metric(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        self.record_metric_row(run_id, phase, elapsed_ms, item_count)
    }

    fn begin_verify_subject(
        &self,
        subject_id: &str,
        subject: VerifySubject,
    ) -> Result<String, String> {
        if subject == VerifySubject::Execution {
            return self.begin_verify(subject_id);
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        match subject {
            VerifySubject::Execution => unreachable!(),
            VerifySubject::Rollback => ensure_rollback_mutable(&transaction, subject_id)?,
            VerifySubject::Recovery => ensure_recovery_run_mutable(&transaction, subject_id)?,
        }
        let execution_id: String = match subject {
            VerifySubject::Execution => unreachable!(),
            VerifySubject::Rollback => transaction
                .query_row(
                    "SELECT execution_id FROM rollback_runs WHERE id=?1",
                    params![subject_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
            VerifySubject::Recovery => transaction
                .query_row(
                    "SELECT CASE journal.direction
                              WHEN 'apply' THEN journal.attempt_id
                              WHEN 'rollback' THEN (
                                  SELECT execution_id FROM rollback_runs
                                   WHERE id=journal.attempt_id
                              )
                           END
                       FROM recovery_runs recovery
                       JOIN operation_journal journal ON journal.id=recovery.operation_id
                      WHERE recovery.id=?1",
                    params![subject_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?,
        };
        let root_scan_id: String = transaction
            .query_row(
                "SELECT plan.scan_id FROM execution_runs execution
                  JOIN plan_runs plan ON plan.id=execution.plan_id
                 WHERE execution.id=?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let id = Uuid::new_v4().to_string();
        let started_at = now();
        transaction
            .execute(
                "INSERT INTO verify_runs(id,execution_id,subject_kind,subject_id,status,started_at)
             VALUES(?1,?2,?3,?4,'running',?5)",
                params![id, execution_id, subject.as_str(), subject_id, started_at],
            )
            .map_err(|error| error.to_string())?;
        insert_workflow_attempt(
            &transaction,
            &id,
            &root_scan_id,
            &format!("{}:{subject_id}", subject.as_str()),
            "verify",
            Some(subject_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn load_verify_subject_items(
        &self,
        subject_id: &str,
        subject: VerifySubject,
    ) -> Result<Vec<VerifyItem>, String> {
        match subject {
            VerifySubject::Execution => self.load_successful_operations(subject_id),
            VerifySubject::Rollback => self.load_successful_rollback_operations(subject_id),
            VerifySubject::Recovery => self.load_successful_recovery_operations(subject_id),
        }
    }
}

impl RollbackStore for SqliteScanStore {
    fn begin_rollback(&self, execution_id: &str, dry_run: bool) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        ensure_execution_mutable(&transaction, execution_id)?;
        let root_scan_id: String = transaction
            .query_row(
                "SELECT plan.scan_id FROM execution_runs execution
                  JOIN plan_runs plan ON plan.id=execution.plan_id
                 WHERE execution.id=?1",
                params![execution_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let started_at = now();
        let mode = if dry_run { "dry_run" } else { "rollback" };
        let kind = if dry_run {
            "rollback_dry_run"
        } else {
            "rollback"
        };
        transaction.execute("INSERT INTO rollback_runs(id,execution_id,mode,status,started_at) VALUES(?1,?2,?3,'running',?4)", params![id,execution_id,mode,started_at]).map_err(|e|e.to_string())?;
        insert_workflow_attempt(
            &transaction,
            &id,
            &root_scan_id,
            execution_id,
            kind,
            Some(execution_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(id)
    }
    fn acquire_rollback_lease(
        &self,
        execution_id: &str,
        owner_id: &str,
    ) -> Result<MutationLease, String> {
        {
            let conn = self
                .connection
                .lock()
                .map_err(|_| "database mutex poisoned".to_string())?;
            ensure_execution_mutable(&conn, execution_id)?;
        }
        let roots = self.mutation_roots_for_execution(execution_id)?;
        let lease = self.acquire_mutation_lease(&roots, owner_id, None)?;
        let guard = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())
            .and_then(|conn| ensure_execution_mutable(&conn, execution_id));
        if let Err(error) = guard {
            let _ = self.release_mutation_lease(&lease);
            return Err(error);
        }
        Ok(lease)
    }
    fn heartbeat_rollback_lease(&self, lease: &MutationLease) -> Result<(), String> {
        self.heartbeat_mutation_lease(lease)
    }
    fn release_rollback_lease(&self, lease: &MutationLease) -> Result<(), String> {
        self.release_mutation_lease(lease)
    }
    fn prepare_rollback_operation(&self, intent: &OperationIntent) -> Result<(), String> {
        if intent.direction != MutationDirection::Rollback {
            return Err("journal_direction_not_rollback".into());
        }
        self.prepare_journal_operation(intent)
    }
    fn transition_rollback_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.transition_journal_operation(operation_id, from, to, lease, staged, error)
    }
    fn load_rollback_items(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT operation.id,journal.sequence_no,
                        journal.source_path_encoding,journal.source_path_blob,
                        journal.target_path_encoding,journal.target_path_blob,
                        CASE journal.strategy WHEN 'atomic_no_replace_rename' THEN 'move'
                             ELSE 'copy_delete' END,
                        journal.expected_size,journal.expected_content_sha256,
                        CASE journal.strategy WHEN 'copy_publish_delete'
                             THEN COALESCE(journal.staged_file_identity,journal.expected_file_identity)
                             ELSE journal.expected_file_identity END
                   FROM operation_journal journal
                   JOIN operation_logs operation
                     ON operation.execution_id=journal.attempt_id
                    AND operation.sequence_no=journal.sequence_no
                  WHERE journal.attempt_id=?1 AND journal.direction='apply'
                    AND journal.state='completed'
                  UNION ALL
                 SELECT operation.id,operation.sequence_no,
                        operation.source_path_encoding,operation.source_path_blob,
                        operation.target_path_encoding,operation.target_path_blob,
                        operation.action,operation.expected_size,
                        operation.expected_content_sha256,operation.expected_file_identity
                   FROM operation_logs operation
                  WHERE operation.execution_id=?1
                    AND ((operation.result='success'
                          AND operation.action IN ('move','copy_delete'))
                      OR (operation.result='failed'
                          AND operation.action='copy_source_retained'))
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=operation.execution_id
                           AND journal.direction='apply'
                           AND journal.sequence_no=operation.sequence_no
                           AND journal.state='completed'
                    )
                  ORDER BY sequence_no",
            )
            .map_err(|error| error.to_string())?;
        let items = statement
            .query_map(params![execution_id], verify_item_from_row)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(items)
    }
    fn completed_rollback_evidence(
        &self,
        execution_id: &str,
        operation_id: &str,
    ) -> Result<Option<RollbackCompletionEvidence>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        conn.query_row(
            "SELECT rollback.id,journal.expected_size,
                    journal.expected_content_sha256,
                    COALESCE(journal.staged_file_identity,operation.expected_file_identity)
               FROM rollback_runs rollback
               JOIN operation_journal journal
                 ON journal.attempt_id=rollback.id
                AND journal.direction='rollback'
                AND journal.plan_item_id=?2
               JOIN operation_logs operation
                 ON operation.id=?2 AND operation.execution_id=?1
               JOIN rollback_logs result
                 ON result.rollback_run_id=rollback.id
                AND result.operation_id=operation.id
              WHERE rollback.execution_id=?1
                AND rollback.mode='rollback'
                AND rollback.status IN ('completed','partial','recovery_required')
                AND result.result='success'
                AND journal.state='completed'
                AND journal.expected_size IS NOT NULL
                AND journal.expected_content_sha256 IS NOT NULL
              ORDER BY rollback.finished_at DESC,rollback.id DESC
              LIMIT 1",
            params![execution_id, operation_id],
            |row| {
                let size = row.get::<_, i64>(1)?;
                Ok(RollbackCompletionEvidence {
                    rollback_id: row.get(0)?,
                    expected_size: u64::try_from(size)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, size))?,
                    expected_content_sha256: row.get(2)?,
                    restored_file_identity: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())
    }
    fn save_rollback_result(
        &self,
        rollback_id: &str,
        operation_id: &str,
        result: OperationResult,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO rollback_logs(id,execution_id,rollback_run_id,operation_id,result,error,created_at) SELECT ?1,execution_id,id,?3,?4,?5,?6 FROM rollback_runs WHERE id=?2", params![Uuid::new_v4().to_string(),rollback_id,operation_id,result.as_str(),error,now()]).map_err(|e|e.to_string())?;
        Ok(())
    }
    fn finish_rollback(
        &self,
        rollback_id: &str,
        status: RunStatus,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let finished_at = now();
        reconcile_terminal_journal_summaries(&transaction, finished_at)?;
        let authoritative = terminal_rollback_journal_outcome(&transaction, rollback_id)?;
        let (effective_status, success, skipped, failed) = if matches!(
            status,
            RunStatus::Completed | RunStatus::Partial | RunStatus::RecoveryRequired
        ) {
            authoritative.map_or((status, success, skipped, failed), |outcome| {
                (
                    outcome.status,
                    outcome.success,
                    outcome.skipped,
                    outcome.failed,
                )
            })
        } else {
            (status, success, skipped, failed)
        };
        let changed = transaction.execute("UPDATE rollback_runs SET status=?2,finished_at=?3,success_count=?4,skipped_count=?5,failed_count=?6 WHERE id=?1 AND status='running'",params![rollback_id,effective_status.as_str(),finished_at,success as i64,skipped as i64,failed as i64]).map_err(|e|e.to_string())?;
        if changed != 1 {
            return Err("rollback_not_running".into());
        }
        let kind: String = transaction
            .query_row(
                "SELECT kind FROM workflow_attempts WHERE id=?1",
                params![rollback_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        finish_workflow_attempt(
            &transaction,
            rollback_id,
            &kind,
            effective_status,
            finished_at,
            success,
            skipped,
            failed,
            None,
        )?;
        transaction.commit().map_err(|error| error.to_string())
    }
    fn record_metric(
        &self,
        run_id: &str,
        phase: &str,
        elapsed_ms: u64,
        item_count: u64,
    ) -> Result<(), String> {
        self.record_metric_row(run_id, phase, elapsed_ms, item_count)
    }
}

type RecoveryRow = (
    String,
    String,
    i64,
    String,
    String,
    String,
    Vec<u8>,
    String,
    Vec<u8>,
    String,
    Vec<u8>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn decode_recovery_row(row: RecoveryRow) -> Result<RecoveryItem, String> {
    let (
        operation_id,
        attempt_id,
        sequence_no,
        state,
        strategy,
        source_encoding,
        source_blob,
        target_encoding,
        target_blob,
        temporary_encoding,
        temporary_blob,
        expected_size,
        expected_content_sha256,
        expected_file_identity,
        staged_file_identity,
    ) = row;
    let state = journal_state_from_code(&state)?;
    let strategy = MutationStrategy::from_code(&strategy)
        .ok_or_else(|| format!("journal_strategy_unknown:{strategy}"))?;
    let suggested_action = match (strategy, state) {
        (MutationStrategy::AtomicNoReplaceRename, JournalState::Prepared)
        | (MutationStrategy::AtomicNoReplaceRename, JournalState::ContentVerified)
        | (MutationStrategy::AtomicNoReplaceRename, JournalState::Published)
        | (MutationStrategy::AtomicNoReplaceRename, JournalState::SourceDeleted)
        | (MutationStrategy::AtomicNoReplaceRename, JournalState::RecoveryRequired) => {
            RecoveryAction::Resume
        }
        (MutationStrategy::AtomicNoReplaceRename, JournalState::Staging | JournalState::Staged) => {
            RecoveryAction::ManualIntervention
        }
        (MutationStrategy::CopyPublishDelete, JournalState::Prepared) => RecoveryAction::Resume,
        (MutationStrategy::CopyPublishDelete, JournalState::Staging | JournalState::Staged) => {
            RecoveryAction::DiscardUnpublishedTemporary
        }
        (
            MutationStrategy::ConditionalDeleteOnly,
            JournalState::Prepared
            | JournalState::Staging
            | JournalState::Staged
            | JournalState::ContentVerified
            | JournalState::Published
            | JournalState::SourceDeleted
            | JournalState::RecoveryRequired,
        ) => RecoveryAction::ManualIntervention,
        (
            MutationStrategy::CopyPublishDelete,
            JournalState::ContentVerified
            | JournalState::Published
            | JournalState::SourceDeleted
            | JournalState::RecoveryRequired,
        ) => RecoveryAction::Resume,
        (_, JournalState::Completed | JournalState::Failed) => {
            return Err("terminal_journal_in_recovery_query".into());
        }
    };
    Ok(RecoveryItem {
        operation_id,
        attempt_id,
        sequence_no: u64::try_from(sequence_no)
            .map_err(|_| "journal_sequence_invalid".to_string())?,
        state,
        strategy,
        source: decode_path(&source_encoding, &source_blob)?,
        target: decode_path(&target_encoding, &target_blob)?,
        temporary: decode_path(&temporary_encoding, &temporary_blob)?,
        expected_size: expected_size
            .map(|value| {
                u64::try_from(value).map_err(|_| "journal_expected_size_invalid".to_string())
            })
            .transpose()?,
        expected_content_sha256,
        expected_file_identity,
        staged_file_identity,
        suggested_action,
    })
}

impl RecoveryStore for SqliteScanStore {
    fn list_recovery_items(&self) -> Result<Vec<RecoveryItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT journal.id,journal.attempt_id,journal.sequence_no,journal.state,
                        journal.strategy,
                        journal.source_path_encoding,journal.source_path_blob,
                        journal.target_path_encoding,journal.target_path_blob,
                        journal.temporary_path_encoding,journal.temporary_path_blob,
                        journal.expected_size,journal.expected_content_sha256,
                        journal.expected_file_identity,journal.staged_file_identity
                   FROM operation_journal journal
                  WHERE journal.state NOT IN ('completed','failed')
                    AND (
                        instr(COALESCE(journal.error,''),'mutation_lease_abandoned')>0
                        OR NOT EXISTS (
                            SELECT 1 FROM mutation_leases lease
                             WHERE lease.fencing_token=journal.fencing_token
                        )
                    )
                  ORDER BY journal.updated_at,journal.sequence_no,journal.id",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<RecoveryRow>, _>>()
            .map_err(|error| error.to_string())?;
        drop(statement);
        drop(conn);

        rows.into_iter().map(decode_recovery_row).collect()
    }

    fn begin_recovery(
        &self,
        operation_id: &str,
        action: RecoveryAction,
        dry_run: bool,
    ) -> Result<String, String> {
        let recovery_id = Uuid::new_v4().to_string();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        ensure_recovery_operation_mutable(&transaction, operation_id)?;
        let context: (String, String) = transaction
            .query_row(
                "SELECT journal.attempt_id,attempt.root_scan_id
                   FROM operation_journal journal
                   JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id
                  WHERE journal.id=?1",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let started_at = now();
        let mode = if dry_run { "dry_run" } else { "recovery" };
        let kind = if dry_run {
            "recovery_dry_run"
        } else {
            "recovery"
        };
        let changed = transaction
            .execute(
                "INSERT INTO recovery_runs(id,operation_id,mode,action,status,started_at)
                 SELECT ?1,id,?3,?4,'running',?5 FROM operation_journal
                  WHERE id=?2 AND state NOT IN ('completed','failed')",
                params![recovery_id, operation_id, mode, action.as_str(), started_at],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("recovery_operation_not_available".into());
        }
        insert_workflow_attempt(
            &transaction,
            &recovery_id,
            &context.1,
            operation_id,
            kind,
            Some(&context.0),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(recovery_id)
    }

    fn acquire_recovery_lease(
        &self,
        operation_id: &str,
        owner_id: &str,
    ) -> Result<MutationLease, String> {
        {
            let conn = self
                .connection
                .lock()
                .map_err(|_| "database mutex poisoned".to_string())?;
            ensure_recovery_operation_mutable(&conn, operation_id)?;
        }
        let roots = self.mutation_roots_for_operation(operation_id)?;
        let lease = self.acquire_mutation_lease(&roots, owner_id, Some(operation_id))?;
        let ownership = match self.lease_ownership(&lease) {
            Ok(ownership) => ownership,
            Err(error) => {
                let _ = self.release_mutation_lease(&lease);
                return Err(error);
            }
        };
        let current_time = now();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        if renew_complete_mutation_lease(
            &transaction,
            lease.fencing_token,
            &ownership,
            current_time,
        )
        .is_err()
        {
            drop(transaction);
            drop(connection);
            let _ = self.release_mutation_lease(&lease);
            return Err("recovery_operation_fenced".into());
        }
        let previous: Option<(String, i64, i64)> = transaction
            .query_row(
                "SELECT state,fencing_token,transition_no
                   FROM operation_journal WHERE id=?1
                    AND state NOT IN ('completed','failed')",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some((state, previous_fencing_token, previous_transition_no)) = previous else {
            drop(transaction);
            drop(connection);
            let _ = self.release_mutation_lease(&lease);
            return Err("recovery_operation_fenced".into());
        };
        let changed = transaction
            .execute(
                "UPDATE operation_journal
                    SET fencing_token=?2,transition_no=transition_no+1,updated_at=?3
                  WHERE id=?1 AND state=?4 AND fencing_token=?5 AND transition_no=?6
                    AND NOT EXISTS (
                        SELECT 1 FROM mutation_leases other
                         WHERE other.fencing_token=operation_journal.fencing_token
                           AND other.fencing_token<>?2 AND other.expires_at>?3
                    )",
                params![
                    operation_id,
                    lease.fencing_token as i64,
                    current_time,
                    state,
                    previous_fencing_token,
                    previous_transition_no
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed == 1 {
            transaction
                .execute(
                    "INSERT INTO operation_journal_events(
                         operation_id,transition_no,from_state,to_state,fencing_token,
                         payload_schema_version,payload_json,created_at
                     ) VALUES(?1,?2,?3,?3,?4,1,?5,?6)",
                    params![
                        operation_id,
                        previous_transition_no + 1,
                        state,
                        lease.fencing_token as i64,
                        serde_json::json!({
                            "event": "recovery_refenced",
                            "previous_fencing_token": previous_fencing_token
                        })
                        .to_string(),
                        current_time
                    ],
                )
                .map_err(|error| error.to_string())?;
            transaction.commit().map_err(|error| error.to_string())?;
            Ok(lease)
        } else {
            drop(transaction);
            drop(connection);
            let _ = self.release_mutation_lease(&lease);
            Err("recovery_operation_fenced".into())
        }
    }

    fn load_recovery_item_under_lease(
        &self,
        operation_id: &str,
        lease: &MutationLease,
    ) -> Result<RecoveryItem, String> {
        let ownership = self
            .lease_ownership(lease)
            .map_err(|_| "recovery_item_not_owned".to_string())?;
        let current_time = now();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        renew_complete_mutation_lease(&transaction, lease.fencing_token, &ownership, current_time)
            .map_err(|_| "recovery_item_not_owned".to_string())?;
        ensure_recovery_operation_mutable(&transaction, operation_id)?;
        let row: RecoveryRow = transaction
            .query_row(
                "SELECT journal.id,journal.attempt_id,journal.sequence_no,journal.state,
                        journal.strategy,
                        journal.source_path_encoding,journal.source_path_blob,
                        journal.target_path_encoding,journal.target_path_blob,
                        journal.temporary_path_encoding,journal.temporary_path_blob,
                        journal.expected_size,journal.expected_content_sha256,
                        journal.expected_file_identity,journal.staged_file_identity
                   FROM operation_journal journal
                  WHERE journal.id=?1 AND journal.state NOT IN ('completed','failed')
                    AND journal.fencing_token=?2
                ",
                params![
                    operation_id,
                    i64::try_from(lease.fencing_token)
                        .map_err(|_| "mutation_fencing_token_invalid")?
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                        row.get(14)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recovery_item_not_owned".to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        decode_recovery_row(row)
    }

    fn heartbeat_recovery_lease(&self, lease: &MutationLease) -> Result<(), String> {
        self.heartbeat_mutation_lease(lease)
    }

    fn transition_recovery_operation(
        &self,
        operation_id: &str,
        from: JournalState,
        to: JournalState,
        lease: &MutationLease,
        staged: Option<&StagedFile>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.transition_journal_operation(operation_id, from, to, lease, staged, error)
    }

    fn finish_recovery(
        &self,
        recovery_id: &str,
        status: RunStatus,
        error: Option<&str>,
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|database_error| database_error.to_string())?;
        let finished_at = now();
        // Recovery can be the step that advances an apply/rollback journal to
        // Completed. Rebuild its denormalized summary in the same transaction
        // so a successful recovery cannot leave rollback/verify inputs orphaned.
        reconcile_terminal_journal_summaries(&transaction, finished_at)?;
        let changed = transaction
            .execute(
                "UPDATE recovery_runs
                    SET status=?2,finished_at=?3,error=?4
                  WHERE id=?1 AND status='running'",
                params![recovery_id, status.as_str(), finished_at, error],
            )
            .map_err(|database_error| database_error.to_string())?;
        if changed != 1 {
            return Err("recovery_attempt_not_running".into());
        }
        let kind: String = transaction
            .query_row(
                "SELECT kind FROM workflow_attempts WHERE id=?1",
                params![recovery_id],
                |row| row.get(0),
            )
            .map_err(|database_error| database_error.to_string())?;
        finish_workflow_attempt(
            &transaction,
            recovery_id,
            &kind,
            status,
            finished_at,
            u64::from(status == RunStatus::Completed),
            0,
            u64::from(matches!(
                status,
                RunStatus::Failed | RunStatus::Partial | RunStatus::RecoveryRequired
            )),
            error,
        )?;
        if status == RunStatus::Completed {
            let recovered_parent = transaction
                .query_row(
                    "SELECT journal.attempt_id,journal.direction
                       FROM recovery_runs recovery
                       JOIN operation_journal journal ON journal.id=recovery.operation_id
                      WHERE recovery.id=?1 AND recovery.mode='recovery'
                        AND journal.state IN ('completed','failed')",
                    params![recovery_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|database_error| database_error.to_string())?;
            if let Some((parent_attempt_id, direction)) = recovered_parent {
                match direction.as_str() {
                    "apply" => {
                        persist_terminal_apply_outcome(
                            &transaction,
                            &parent_attempt_id,
                            finished_at,
                        )?;
                    }
                    "rollback" => {
                        persist_terminal_rollback_outcome(
                            &transaction,
                            &parent_attempt_id,
                            finished_at,
                        )?;
                    }
                    _ => {}
                }
            }
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    fn release_recovery_lease(&self, lease: &MutationLease) -> Result<(), String> {
        self.release_mutation_lease(lease)
    }
}
