use super::{
    now, DiagnosticEventInput, DiagnosticEventRow, DiagnosticExportResult, DiagnosticPresentation,
    DiagnosticRetentionPolicy, DiagnosticRetentionPreview, DiagnosticRetentionResult,
    SqliteScanStore,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const DIAGNOSTIC_EXPORT_SCHEMA_VERSION: u32 = 1;
const MIN_PROGRESS_DEBUG_DAYS: u32 = 7;
const MIN_DIAGNOSTIC_DAYS: u32 = 30;
const MIN_RECOVERY_AUDIT_DAYS: u32 = 180;

impl SqliteScanStore {
    pub fn record_diagnostic(&self, event: DiagnosticEventInput) -> Result<String, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        insert_diagnostic(&connection, event)
    }

    pub fn list_diagnostics(
        &self,
        correlation: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DiagnosticEventRow>, String> {
        if !(1..=500).contains(&limit) {
            return Err("diagnostic_page_limit_invalid".into());
        }
        type DiagnosticTuple = (
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            String,
            Option<String>,
            i64,
            i64,
            i64,
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id,run_id,attempt_id,class,severity,phase,code,item_id,
                        sequence_no,path_role,message_key,payload_json,
                        contains_sensitive_path,protected,created_at
                   FROM diagnostic_events
                  WHERE ?1 IS NULL OR run_id=?1 OR attempt_id=?1
                  ORDER BY created_at DESC,id DESC LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        let values = statement
            .query_map(params![correlation, limit as i64], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, Option<String>>(4)?
                        .unwrap_or_else(|| "warning".into()),
                    row.get::<_, Option<String>>(5)?
                        .unwrap_or_else(|| "unknown".into()),
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get::<_, Option<String>>(10)?
                        .unwrap_or_else(|| "diagnostic_unknown".into()),
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<DiagnosticTuple>, _>>()
            .map_err(|error| error.to_string())?;
        values
            .into_iter()
            .map(
                |(
                    correlation_id,
                    run_id,
                    attempt_id,
                    class,
                    severity,
                    phase,
                    code,
                    item_id,
                    sequence_no,
                    path_role,
                    message_key,
                    payload_json,
                    contains_sensitive_path,
                    protected,
                    created_at,
                )| {
                    let payload = payload_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(|_| "diagnostic_payload_invalid".to_string())?
                        .unwrap_or(Value::Null);
                    Ok(DiagnosticEventRow {
                        presentation: diagnostic_presentation_ja(&code, &correlation_id),
                        correlation_id,
                        run_id,
                        attempt_id,
                        class,
                        severity,
                        phase,
                        code,
                        item_id,
                        sequence_no: sequence_no
                            .map(|value| {
                                u64::try_from(value)
                                    .map_err(|_| "diagnostic_sequence_invalid".to_string())
                            })
                            .transpose()?,
                        path_role,
                        message_key,
                        payload,
                        contains_sensitive_path: contains_sensitive_path != 0,
                        protected: protected != 0,
                        created_at,
                    })
                },
            )
            .collect()
    }

    pub fn run_diagnostic_retention(
        &self,
        policy: &DiagnosticRetentionPolicy,
    ) -> Result<DiagnosticRetentionResult, String> {
        self.run_diagnostic_retention_at(policy, now())
    }

    pub fn preview_diagnostic_retention(
        &self,
        policy: &DiagnosticRetentionPolicy,
    ) -> Result<DiagnosticRetentionPreview, String> {
        self.preview_diagnostic_retention_at(policy, now())
    }

    pub fn preview_diagnostic_retention_at(
        &self,
        policy: &DiagnosticRetentionPolicy,
        reference_time: i64,
    ) -> Result<DiagnosticRetentionPreview, String> {
        validate_retention_policy(policy)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let eligible_progress_debug = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostic_events WHERE protected=0 AND class IN ('progress','debug') AND created_at<?1",
                params![cutoff(reference_time, policy.progress_debug_days)],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as u64;
        let eligible_diagnostic = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostic_events event
                  WHERE event.protected=0 AND event.class='diagnostic'
                    AND event.created_at<?1
                    AND NOT EXISTS(
                        SELECT 1 FROM workflow_attempts attempt
                         WHERE attempt.id=COALESCE(event.attempt_id,event.run_id)
                           AND attempt.status IN ('running','recovery_required')
                    )
                    AND NOT EXISTS(
                        SELECT 1 FROM operation_journal journal
                         WHERE journal.attempt_id=COALESCE(event.attempt_id,event.run_id)
                           AND journal.state NOT IN ('completed','failed')
                    )",
                params![cutoff(reference_time, policy.diagnostic_days)],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as u64;
        let eligible_recovery_audit = connection
            .query_row(
                "SELECT COUNT(*)
                   FROM diagnostic_events event
                  WHERE event.protected=0
                    AND event.class IN ('audit','recovery')
                    AND event.created_at<?1
                    AND EXISTS(
                        SELECT 1 FROM archive_manifests archive
                         WHERE archive.status='completed'
                           AND archive.root_scan_id=COALESCE(
                               (SELECT attempt.root_scan_id FROM workflow_attempts attempt WHERE attempt.id=COALESCE(event.attempt_id,event.run_id)),
                               (SELECT scan.id FROM scan_runs scan WHERE scan.id=event.run_id),
                               (SELECT plan.scan_id FROM plan_runs plan WHERE plan.id=event.run_id),
                               (SELECT plan.scan_id FROM execution_runs execution JOIN plan_runs plan ON plan.id=execution.plan_id WHERE execution.id=COALESCE(event.attempt_id,event.run_id)),
                               (SELECT plan.scan_id FROM verify_runs verify JOIN execution_runs execution ON execution.id=verify.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE verify.id=COALESCE(event.attempt_id,event.run_id)),
                               (SELECT plan.scan_id FROM rollback_runs rollback JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE rollback.id=COALESCE(event.attempt_id,event.run_id))
                           )
                    )",
                params![cutoff(reference_time, policy.recovery_audit_days)],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as u64;
        let protected_remaining = connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostic_events event
                  WHERE event.protected=1 OR event.class IN ('audit','recovery')
                     OR EXISTS(SELECT 1 FROM workflow_attempts attempt
                                WHERE attempt.id=COALESCE(event.attempt_id,event.run_id)
                                  AND attempt.status IN ('running','recovery_required'))
                     OR EXISTS(SELECT 1 FROM operation_journal journal
                                WHERE journal.attempt_id=COALESCE(event.attempt_id,event.run_id)
                                  AND journal.state NOT IN ('completed','failed'))",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as u64;
        drop(connection);
        let database_bytes = database_file_bytes(&self.database_path)?;
        let capacity_exceeded = policy
            .max_database_bytes
            .is_some_and(|maximum| database_bytes > maximum);
        Ok(DiagnosticRetentionPreview {
            eligible_progress_debug,
            eligible_diagnostic,
            eligible_recovery_audit,
            protected_remaining,
            batch_size: policy.batch_size,
            database_bytes,
            capacity_exceeded,
        })
    }

    pub fn run_diagnostic_retention_at(
        &self,
        policy: &DiagnosticRetentionPolicy,
        reference_time: i64,
    ) -> Result<DiagnosticRetentionResult, String> {
        validate_retention_policy(policy)?;
        let retention_run_id = Uuid::new_v4().to_string();
        let progress_cutoff = cutoff(reference_time, policy.progress_debug_days);
        let diagnostic_cutoff = cutoff(reference_time, policy.diagnostic_days);
        let recovery_cutoff = cutoff(reference_time, policy.recovery_audit_days);
        let cutoff_json = json!({
            "progress_debug": progress_cutoff,
            "diagnostic": diagnostic_cutoff,
            "recovery_audit": recovery_cutoff,
        })
        .to_string();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        connection
            .execute(
                "INSERT INTO retention_runs(id,policy_version,cutoff_by_class_json,status,started_at) VALUES(?1,1,?2,'running',?3)",
                params![retention_run_id, cutoff_json, reference_time],
            )
            .map_err(|error| error.to_string())?;

        let outcome = (|| {
            let transaction = connection
                .transaction()
                .map_err(|error| error.to_string())?;
            let batch_size = i64::from(policy.batch_size);
            let deleted_progress_debug = transaction
                .execute(
                    "DELETE FROM diagnostic_events WHERE id IN (
                         SELECT id FROM diagnostic_events
                          WHERE protected=0 AND class IN ('progress','debug') AND created_at<?1
                          ORDER BY created_at,id LIMIT ?2
                     )",
                    params![progress_cutoff, batch_size],
                )
                .map_err(|error| error.to_string())?
                as u64;
            let deleted_diagnostic = transaction
                .execute(
                    "DELETE FROM diagnostic_events WHERE id IN (
                         SELECT event.id FROM diagnostic_events event
                          WHERE event.protected=0 AND event.class='diagnostic'
                            AND event.created_at<?1
                            AND NOT EXISTS(
                                SELECT 1 FROM workflow_attempts attempt
                                 WHERE attempt.id=COALESCE(event.attempt_id,event.run_id)
                                   AND attempt.status IN ('running','recovery_required')
                            )
                            AND NOT EXISTS(
                                SELECT 1 FROM operation_journal journal
                                 WHERE journal.attempt_id=COALESCE(event.attempt_id,event.run_id)
                                   AND journal.state NOT IN ('completed','failed')
                            )
                          ORDER BY event.created_at,event.id LIMIT ?2
                     )",
                    params![diagnostic_cutoff, batch_size],
                )
                .map_err(|error| error.to_string())? as u64;
            let deleted_recovery_audit = transaction
                .execute(
                    "DELETE FROM diagnostic_events WHERE id IN (
                         SELECT event.id
                           FROM diagnostic_events event
                          WHERE event.protected=0
                            AND event.class IN ('audit','recovery')
                            AND event.created_at<?1
                            AND EXISTS(
                                SELECT 1 FROM archive_manifests archive
                                 WHERE archive.status='completed'
                                   AND archive.root_scan_id=COALESCE(
                                       (SELECT attempt.root_scan_id FROM workflow_attempts attempt WHERE attempt.id=COALESCE(event.attempt_id,event.run_id)),
                                       (SELECT scan.id FROM scan_runs scan WHERE scan.id=event.run_id),
                                       (SELECT plan.scan_id FROM plan_runs plan WHERE plan.id=event.run_id),
                                       (SELECT plan.scan_id FROM execution_runs execution JOIN plan_runs plan ON plan.id=execution.plan_id WHERE execution.id=COALESCE(event.attempt_id,event.run_id)),
                                       (SELECT plan.scan_id FROM verify_runs verify JOIN execution_runs execution ON execution.id=verify.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE verify.id=COALESCE(event.attempt_id,event.run_id)),
                                       (SELECT plan.scan_id FROM rollback_runs rollback JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE rollback.id=COALESCE(event.attempt_id,event.run_id)),
                                       (SELECT plan.scan_id FROM recovery_runs recovery JOIN operation_journal journal ON journal.id=recovery.operation_id JOIN execution_runs execution ON execution.id=journal.attempt_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE recovery.id=COALESCE(event.attempt_id,event.run_id))
                                   )
                            )
                          ORDER BY event.created_at,event.id LIMIT ?2
                     )",
                    params![recovery_cutoff, batch_size],
                )
                .map_err(|error| error.to_string())? as u64;
            let protected_remaining = transaction
                .query_row(
                    "SELECT COUNT(*) FROM diagnostic_events event
                      WHERE event.protected=1 OR event.class IN ('audit','recovery')
                         OR EXISTS(SELECT 1 FROM workflow_attempts attempt
                                    WHERE attempt.id=COALESCE(event.attempt_id,event.run_id)
                                      AND attempt.status IN ('running','recovery_required'))
                         OR EXISTS(SELECT 1 FROM operation_journal journal
                                    WHERE journal.attempt_id=COALESCE(event.attempt_id,event.run_id)
                                      AND journal.state NOT IN ('completed','failed'))",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?
                .max(0) as u64;
            let deleted_counts_json = json!({
                "progress_debug": deleted_progress_debug,
                "diagnostic": deleted_diagnostic,
                "recovery_audit": deleted_recovery_audit,
            })
            .to_string();
            transaction
                .execute(
                    "UPDATE retention_runs SET deleted_counts_json=?2,status='completed',finished_at=?3 WHERE id=?1 AND status='running'",
                    params![retention_run_id, deleted_counts_json, reference_time],
                )
                .map_err(|error| error.to_string())?;
            transaction.commit().map_err(|error| error.to_string())?;
            Ok::<_, String>((
                deleted_progress_debug,
                deleted_diagnostic,
                deleted_recovery_audit,
                protected_remaining,
            ))
        })();

        let (
            deleted_progress_debug,
            deleted_diagnostic,
            deleted_recovery_audit,
            protected_remaining,
        ) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = connection.execute(
                    "UPDATE retention_runs SET status='failed',finished_at=?2,error=?3 WHERE id=?1 AND status='running'",
                    params![retention_run_id, reference_time, error],
                );
                return Err(error);
            }
        };
        drop(connection);
        let measured_bytes = database_file_bytes(&self.database_path)?;
        let measured_capacity_exceeded = policy
            .max_database_bytes
            .is_some_and(|maximum| measured_bytes > maximum);
        // The configured age minima are safety floors. A byte cap never
        // evicts fresh, protected, active-attempt, or recovery evidence; an
        // over-cap state is explicit and auditable for operator action.
        let _ = self.record_diagnostic(DiagnosticEventInput {
            run_id: Some(retention_run_id.clone()),
            attempt_id: None,
            class: "audit".into(),
            severity: if measured_capacity_exceeded {
                "warning".into()
            } else {
                "info".into()
            },
            phase: "retention".into(),
            code: if measured_capacity_exceeded {
                "diagnostic_capacity_exceeded".into()
            } else {
                "diagnostic_retention_completed".into()
            },
            item_id: None,
            sequence_no: None,
            path_role: None,
            message_key: if measured_capacity_exceeded {
                "diagnostic_capacity_exceeded".into()
            } else {
                "diagnostic_retention_completed".into()
            },
            payload: json!({
                "reason": "age_policy",
                "deleted": {
                    "progress_debug": deleted_progress_debug,
                    "diagnostic": deleted_diagnostic,
                    "recovery_audit": deleted_recovery_audit,
                },
                "database_bytes_before_audit": measured_bytes,
                "max_database_bytes": policy.max_database_bytes,
                "capacity_eviction_deferred_by_safety_floor": measured_capacity_exceeded,
            }),
            contains_sensitive_path: false,
            protected: false,
        });
        let database_bytes = database_file_bytes(&self.database_path)?;
        let capacity_exceeded = policy
            .max_database_bytes
            .is_some_and(|maximum| database_bytes > maximum);
        Ok(DiagnosticRetentionResult {
            retention_run_id,
            deleted_progress_debug,
            deleted_diagnostic,
            deleted_recovery_audit,
            protected_remaining,
            database_bytes,
            capacity_exceeded,
        })
    }

    pub fn export_diagnostics(
        &self,
        destination: &Path,
        redact_sensitive_paths: bool,
    ) -> Result<DiagnosticExportResult, String> {
        if destination.exists() {
            return Err("diagnostic_export_target_exists".into());
        }
        let parent = destination
            .parent()
            .ok_or_else(|| "diagnostic_export_parent_missing".to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = temporary_export_path(destination);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        let export_result = (|| {
            let mut writer = BufWriter::new(file);
            let mut digest = Sha256::new();
            let mut byte_count = 0_u64;
            let mut record_count = 0_u64;
            write_json_line(
                &mut writer,
                &mut digest,
                &mut byte_count,
                &json!({
                    "diagnostic_export_schema_version": DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
                    "sensitive_paths_redacted": redact_sensitive_paths,
                }),
            )?;
            let connection = self
                .connection
                .lock()
                .map_err(|_| "database mutex poisoned".to_string())?;
            let mut statement = connection
                .prepare(
                    "SELECT id,run_id,attempt_id,class,severity,phase,code,item_id,sequence_no,path_role,message_key,payload_json,contains_sensitive_path,created_at FROM diagnostic_events ORDER BY created_at,id",
                )
                .map_err(|error| error.to_string())?;
            let mut rows = statement.query([]).map_err(|error| error.to_string())?;
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                let contains_sensitive_path =
                    row.get::<_, i64>(12).map_err(|error| error.to_string())? != 0;
                let raw_payload = row
                    .get::<_, Option<String>>(11)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_else(|| "null".to_string());
                let mut payload: Value = serde_json::from_str(&raw_payload)
                    .unwrap_or_else(|_| Value::String("invalid_payload_redacted".into()));
                scrub_secret_values(&mut payload);
                if redact_sensitive_paths && contains_sensitive_path {
                    payload = json!({"redacted": true});
                }
                let path_role: Option<String> = row.get(9).map_err(|error| error.to_string())?;
                let value = json!({
                    "correlation_id": row.get::<_, String>(0).map_err(|error| error.to_string())?,
                    "run_id": row.get::<_, Option<String>>(1).map_err(|error| error.to_string())?,
                    "attempt_id": row.get::<_, Option<String>>(2).map_err(|error| error.to_string())?,
                    "class": row.get::<_, String>(3).map_err(|error| error.to_string())?,
                    "severity": row.get::<_, Option<String>>(4).map_err(|error| error.to_string())?,
                    "phase": row.get::<_, Option<String>>(5).map_err(|error| error.to_string())?,
                    "code": row.get::<_, String>(6).map_err(|error| error.to_string())?,
                    "item_id": row.get::<_, Option<String>>(7).map_err(|error| error.to_string())?,
                    "sequence_no": row.get::<_, Option<i64>>(8).map_err(|error| error.to_string())?,
                    "path_role": if redact_sensitive_paths && contains_sensitive_path { None } else { path_role },
                    "message_key": row.get::<_, Option<String>>(10).map_err(|error| error.to_string())?,
                    "payload": payload,
                    "created_at": row.get::<_, i64>(13).map_err(|error| error.to_string())?,
                });
                write_json_line(&mut writer, &mut digest, &mut byte_count, &value)?;
                record_count = record_count
                    .checked_add(1)
                    .ok_or_else(|| "diagnostic_export_record_count_overflow".to_string())?;
            }
            drop(rows);
            drop(statement);
            drop(connection);
            writer.flush().map_err(|error| error.to_string())?;
            writer
                .get_ref()
                .sync_all()
                .map_err(|error| error.to_string())?;
            let sha256 = format!("{:x}", digest.finalize());
            fs::hard_link(&temporary, destination).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "diagnostic_export_target_exists".to_string()
                } else {
                    error.to_string()
                }
            })?;
            fs::remove_file(&temporary).map_err(|error| error.to_string())?;
            if let Some(parent) = destination.parent() {
                crate::windows_fs::sync_directory(parent)
                    .map_err(|error| format!("diagnostic_directory_flush_failed:{error}"))?;
            }
            verify_export(destination, &sha256, byte_count)?;
            Ok::<_, String>((record_count, byte_count, sha256))
        })();
        let (record_count, byte_count, sha256) = match export_result {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(format!("diagnostic_export_failed:{error}"));
            }
        };
        Ok(DiagnosticExportResult {
            export_path: destination.to_path_buf(),
            schema_version: DIAGNOSTIC_EXPORT_SCHEMA_VERSION,
            record_count,
            byte_count,
            sha256,
            sensitive_paths_redacted: redact_sensitive_paths,
        })
    }
}

pub(super) fn insert_diagnostic(
    connection: &Connection,
    mut event: DiagnosticEventInput,
) -> Result<String, String> {
    validate_event(&event)?;
    scrub_secret_values(&mut event.payload);
    let id = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO diagnostic_events(
                id,run_id,attempt_id,class,code,payload_json,
                contains_sensitive_path,protected,created_at,severity,phase,item_id,
                sequence_no,path_role,payload_schema_version,message_key
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,1,?15)",
            params![
                id,
                event.run_id,
                event.attempt_id,
                event.class,
                event.code,
                serde_json::to_string(&event.payload).map_err(|error| error.to_string())?,
                i32::from(event.contains_sensitive_path),
                i32::from(event.protected),
                now(),
                event.severity,
                event.phase,
                event.item_id,
                event
                    .sequence_no
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| "diagnostic_sequence_invalid")?,
                event.path_role,
                event.message_key,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(id)
}

pub(super) fn insert_diagnostic_best_effort(connection: &Connection, event: DiagnosticEventInput) {
    let _ = insert_diagnostic(connection, event);
}

pub fn diagnostic_presentation_ja(code: &str, correlation_id: &str) -> DiagnosticPresentation {
    let (summary, impact, action) = if code.contains("path_too_long") || code.contains("long_path")
    {
        (
            "移動先のパスが長すぎます。",
            "安全な上限を超えるため、対象ファイルは変更していません。",
            "保存先や命名規則を短くするか、長いパスを許可する設定を確認して再計画してください。",
        )
    } else if code.starts_with("reparse_")
        || code.contains("outside_root")
        || code.contains("path_traversal")
        || code.starts_with("security_")
    {
        (
            "安全でないパス操作を拒否しました。",
            "指定先の外側やリンク先へ変更が及ぶ可能性があるため、処理していません。",
            "元のファイルを保持したまま、移動元と移動先の設定を確認してください。",
        )
    } else if code.contains("disk_full") || code.contains("no_space") {
        (
            "保存先の空き容量が不足しています。",
            "コピーまたは履歴の保存を完了できず、元のファイルは保持されています。",
            "空き容量を確保してから、同じ実行を安全に再試行してください。",
        )
    } else if code.contains("permission") || code.contains("access_denied") {
        (
            "ファイルまたは保存先へのアクセスが拒否されました。",
            "対象の変更を完了できず、復旧が必要な状態が残る場合があります。",
            "アクセス権と他のアプリによる使用状況を確認し、履歴の復旧候補を確認してください。",
        )
    } else if code.contains("recovery_required")
        || code.starts_with("published_")
        || code.starts_with("recovery_post_commit_")
        || code.starts_with("recovery_delete_failed")
        || code.contains("mutation_lease_abandoned")
    {
        (
            "安全な復旧操作が必要です。",
            "ファイル操作の途中状態が記録されているため、自動的な再実行を停止しました。",
            "復旧候補を確認し、提示された再開または巻き戻しを実行してください。",
        )
    } else if code == "scan_warning" || code.contains("metadata") {
        (
            "読み取り時に警告が記録されました。",
            "一部のファイル情報を利用できない可能性があります。",
            "対象項目を確認し、必要ならメタデータまたはアクセス権を修正して再スキャンしてください。",
        )
    } else if code.ends_with("_failed") || code.contains("error") {
        (
            "処理を完了できませんでした。",
            "該当する実行または項目は失敗として記録されています。",
            "元ファイルを確認し、復旧候補があれば先に解決してから再試行してください。",
        )
    } else {
        (
            "詳細不明の診断が記録されました。",
            "処理結果への影響を自動判定できません。",
            "安全のため操作を繰り返さず、診断を書き出してサポートへ連絡してください。",
        )
    };
    DiagnosticPresentation {
        summary_ja: summary.into(),
        impact_ja: impact.into(),
        next_action_ja: format!("{action} 問い合わせID: {correlation_id}"),
    }
}

fn validate_event(event: &DiagnosticEventInput) -> Result<(), String> {
    if !matches!(
        event.class.as_str(),
        "progress" | "debug" | "diagnostic" | "audit" | "recovery"
    ) {
        return Err("diagnostic_class_invalid".into());
    }
    if !matches!(
        event.severity.as_str(),
        "debug" | "info" | "warning" | "error"
    ) {
        return Err("diagnostic_severity_invalid".into());
    }
    for (name, value) in [
        ("code", event.code.as_str()),
        ("phase", event.phase.as_str()),
        ("message_key", event.message_key.as_str()),
    ] {
        if value.is_empty()
            || !value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(format!("diagnostic_{name}_invalid"));
        }
    }
    Ok(())
}

fn validate_retention_policy(policy: &DiagnosticRetentionPolicy) -> Result<(), String> {
    if policy.progress_debug_days < MIN_PROGRESS_DEBUG_DAYS
        || policy.diagnostic_days < MIN_DIAGNOSTIC_DAYS
        || policy.recovery_audit_days < MIN_RECOVERY_AUDIT_DAYS
    {
        return Err("diagnostic_retention_below_safety_minimum".into());
    }
    if !(1..=10_000).contains(&policy.batch_size) {
        return Err("diagnostic_retention_batch_size_invalid".into());
    }
    Ok(())
}

fn cutoff(reference_time: i64, days: u32) -> i64 {
    reference_time.saturating_sub(i64::from(days).saturating_mul(24 * 60 * 60))
}

fn scrub_secret_values(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let normalized = key.to_ascii_lowercase();
                if [
                    "secret",
                    "token",
                    "password",
                    "credential",
                    "authorization",
                    "cookie",
                ]
                .iter()
                .any(|needle| normalized.contains(needle))
                {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    scrub_secret_values(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                scrub_secret_values(value);
            }
        }
        Value::String(text) if string_may_contain_secret(text) => {
            *text = "[REDACTED]".into();
        }
        _ => {}
    }
}

fn string_may_contain_secret(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    if normalized.contains("bearer ")
        || [
            "password=",
            "passwd=",
            "token=",
            "secret=",
            "authorization=",
            "cookie=",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        return true;
    }
    let Some(scheme) = normalized.find("://") else {
        return false;
    };
    let authority = &normalized[scheme + 3..];
    let authority_end = authority
        .find(['/', '?', '#', ' ', '\n', '\r'])
        .unwrap_or(authority.len());
    authority[..authority_end].contains('@')
}

fn write_json_line(
    writer: &mut BufWriter<File>,
    digest: &mut Sha256,
    byte_count: &mut u64,
    value: &Value,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    digest.update(&bytes);
    *byte_count = byte_count
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| "diagnostic_export_byte_count_overflow".to_string())?;
    Ok(())
}

fn temporary_export_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("diagnostics.jsonl"));
    name.push(format!(".{}.tmp", Uuid::new_v4()));
    destination.with_file_name(name)
}

fn verify_export(path: &Path, expected_sha256: &str, expected_bytes: u64) -> Result<(), String> {
    let mut reader = BufReader::new(File::open(path).map_err(|error| error.to_string())?);
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| "diagnostic_export_byte_count_overflow".to_string())?;
    }
    if bytes != expected_bytes || format!("{:x}", digest.finalize()) != expected_sha256 {
        return Err("diagnostic_export_digest_mismatch".into());
    }
    Ok(())
}

fn database_file_bytes(database_path: &Path) -> Result<u64, String> {
    let mut total = 0_u64;
    for suffix in [None, Some("-wal"), Some("-shm")] {
        let path = match suffix {
            None => database_path.to_path_buf(),
            Some(suffix) => {
                let mut raw = database_path.as_os_str().to_os_string();
                raw.push(suffix);
                PathBuf::from(raw)
            }
        };
        match fs::metadata(path) {
            Ok(metadata) => {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| "database_size_overflow".to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(total)
}
