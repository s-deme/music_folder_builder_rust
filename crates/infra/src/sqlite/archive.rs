use super::{
    finish_workflow_attempt, insert_workflow_attempt, now, HistoryArchiveResult, RunStatus,
    SqliteScanStore, LOCAL_ATTEMPT_ACTOR,
};
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Number, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::path_codec::{decode_path, encode_path};

const ARCHIVE_SCHEMA_VERSION: u32 = 3;

struct ArchiveSink {
    writer: BufWriter<File>,
    digest: Sha256,
    record_count: u64,
    byte_count: u64,
    counts: BTreeMap<String, u64>,
}

impl ArchiveSink {
    fn new(file: File) -> Self {
        Self {
            writer: BufWriter::new(file),
            digest: Sha256::new(),
            record_count: 0,
            byte_count: 0,
            counts: BTreeMap::new(),
        }
    }

    fn write_value(&mut self, entity: Option<&str>, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        self.writer
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        self.digest.update(&bytes);
        self.byte_count = self
            .byte_count
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| "archive_byte_count_overflow".to_string())?;
        if let Some(entity) = entity {
            self.record_count = self
                .record_count
                .checked_add(1)
                .ok_or_else(|| "archive_record_count_overflow".to_string())?;
            *self.counts.entry(entity.to_owned()).or_default() += 1;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(String, u64, u64, BTreeMap<String, u64>), String> {
        self.writer.flush().map_err(|error| error.to_string())?;
        self.writer
            .get_ref()
            .sync_all()
            .map_err(|error| error.to_string())?;
        Ok((
            format!("{:x}", self.digest.finalize()),
            self.record_count,
            self.byte_count,
            self.counts,
        ))
    }
}

pub(super) fn root_scan_id_for(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<String, String> {
    let sql = match kind {
        "scan" => "SELECT id FROM scan_runs WHERE id=?1",
        "plan" => "SELECT scan_id FROM plan_runs WHERE id=?1",
        "apply" => {
            "SELECT plan.scan_id FROM execution_runs execution JOIN plan_runs plan ON plan.id=execution.plan_id WHERE execution.id=?1"
        }
        "verify" => {
            "SELECT plan.scan_id FROM verify_runs verify JOIN execution_runs execution ON execution.id=verify.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE verify.id=?1"
        }
        "rollback" => {
            "SELECT plan.scan_id FROM rollback_runs rollback JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE rollback.id=?1"
        }
        "recovery" => "SELECT root_scan_id FROM workflow_attempts WHERE id=?1 AND kind IN ('recovery','recovery_dry_run')",
        "archive" => "SELECT root_scan_id FROM archive_manifests WHERE id=?1",
        _ => return Err("invalid_run_kind".into()),
    };
    connection
        .query_row(sql, params![id], |row| row.get(0))
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "history_run_not_found".to_string())
}

pub(super) fn archive_block_reasons(
    connection: &Connection,
    root_scan_id: &str,
) -> Result<Vec<String>, String> {
    let mut reasons = Vec::new();
    let closure = "WITH plans(id) AS (
                         SELECT id FROM plan_runs WHERE scan_id=?1
                     ),
                     executions(id) AS (
                         SELECT id FROM execution_runs
                          WHERE plan_id IN (SELECT id FROM plans)
                     ),
                     attempts(id) AS (
                         SELECT id FROM workflow_attempts WHERE root_scan_id=?1
                     ),
                     successful_apply(
                         execution_id,plan_item_id,sequence_no,operation_id
                     ) AS (
                         SELECT journal.attempt_id,journal.plan_item_id,
                                journal.sequence_no,
                                (SELECT summary.id FROM operation_logs summary
                                  WHERE summary.execution_id=journal.attempt_id
                                    AND summary.sequence_no=journal.sequence_no
                                  ORDER BY summary.id LIMIT 1)
                           FROM operation_journal journal
                          WHERE journal.attempt_id IN (SELECT id FROM executions)
                            AND journal.direction='apply'
                            AND journal.state='completed'
                         UNION ALL
                         SELECT summary.execution_id,summary.plan_item_id,
                                summary.sequence_no,summary.id
                           FROM operation_logs summary
                          WHERE summary.execution_id IN (SELECT id FROM executions)
                            AND summary.result='success' AND summary.source_deleted=1
                            AND NOT EXISTS(
                                SELECT 1 FROM operation_journal journal
                                 WHERE journal.attempt_id=summary.execution_id
                                   AND journal.direction='apply'
                                   AND journal.sequence_no=summary.sequence_no
                            )
                     ),
                     completed_rollback(
                         execution_id,referenced_operation_id,rollback_id
                     ) AS (
                         SELECT rollback.execution_id,journal.plan_item_id,rollback.id
                           FROM operation_journal journal
                           JOIN rollback_runs rollback ON rollback.id=journal.attempt_id
                          WHERE rollback.execution_id IN (SELECT id FROM executions)
                            AND rollback.mode='rollback'
                            AND rollback.status='completed'
                            AND rollback.failed_count=0
                            AND journal.direction='rollback'
                            AND journal.state='completed'
                     ) ";
    let checks = [
        (
            "legal_hold",
            "SELECT EXISTS(SELECT 1 FROM scan_runs WHERE id=?1 AND legal_hold=1) OR EXISTS(SELECT 1 FROM plan_runs WHERE scan_id=?1 AND legal_hold=1)",
        ),
        (
            "running_attempt",
            "SELECT EXISTS(SELECT 1 FROM workflow_attempts WHERE root_scan_id=?1 AND status='running') OR EXISTS(SELECT 1 FROM preflight_runs WHERE parent_attempt_id IN (SELECT id FROM attempts) AND status='running') OR EXISTS(SELECT 1 FROM scan_runs WHERE id=?1 AND status='running') OR EXISTS(SELECT 1 FROM plan_runs WHERE scan_id=?1 AND status='running') OR EXISTS(SELECT 1 FROM execution_runs WHERE plan_id IN (SELECT id FROM plans) AND status='running') OR EXISTS(SELECT 1 FROM verify_runs WHERE execution_id IN (SELECT id FROM executions) AND status='running') OR EXISTS(SELECT 1 FROM rollback_runs WHERE execution_id IN (SELECT id FROM executions) AND status='running')",
        ),
        (
            "recovery_required",
            "SELECT EXISTS(
                 SELECT 1 FROM operation_journal
                  WHERE attempt_id IN (SELECT id FROM attempts)
                    AND state NOT IN ('completed','failed')
             )",
        ),
        (
            "unrolled_mutation",
            "SELECT EXISTS(
                 SELECT 1 FROM successful_apply applied
                  WHERE applied.operation_id IS NULL OR NOT EXISTS(
                        SELECT 1 FROM completed_rollback rollback
                         WHERE rollback.execution_id=applied.execution_id
                           AND rollback.referenced_operation_id=applied.operation_id
                  )
             )",
        ),
        (
            "rollback_unverified",
            "SELECT EXISTS(
                 SELECT 1 FROM successful_apply applied
                  WHERE applied.operation_id IS NOT NULL
                    AND EXISTS(
                        SELECT 1 FROM completed_rollback rollback
                         WHERE rollback.execution_id=applied.execution_id
                           AND rollback.referenced_operation_id=applied.operation_id
                    )
                    AND NOT EXISTS(
                        SELECT 1 FROM completed_rollback rollback
                        JOIN verify_runs verify
                          ON verify.subject_kind='rollback'
                         AND verify.subject_id=rollback.rollback_id
                         AND verify.status='completed'
                         AND verify.failed_count=0
                         WHERE rollback.execution_id=applied.execution_id
                           AND rollback.referenced_operation_id=applied.operation_id
                    )
             )",
        ),
        (
            "active_mutation_lease",
            "SELECT EXISTS(
                 SELECT 1 FROM mutation_leases lease
                  WHERE EXISTS(
                        SELECT 1 FROM plan_runs plan
                         WHERE plan.scan_id=?1
                           AND plan.target_root_encoding=lease.scope_path_encoding
                           AND plan.target_root_blob=lease.scope_path_blob
                  ) OR EXISTS(
                        SELECT 1 FROM scan_runs scan
                         WHERE scan.id=?1
                           AND scan.source_root_encoding=lease.scope_path_encoding
                           AND scan.source_root_blob=lease.scope_path_blob
                  )
             )",
        ),
        (
            "active_recovery",
            "SELECT EXISTS(SELECT 1 FROM recovery_runs recovery JOIN operation_journal journal ON journal.id=recovery.operation_id WHERE journal.attempt_id IN (SELECT id FROM attempts) AND recovery.status='running')",
        ),
        (
            "plan_build_in_progress",
            "SELECT EXISTS(SELECT 1 FROM plan_builds build JOIN plan_runs plan ON plan.id=build.plan_id WHERE plan.scan_id=?1 AND build.status='building')",
        ),
    ];
    for (code, query) in checks {
        let sql = format!("{closure}{query}");
        let blocked: i64 = connection
            .query_row(&sql, params![root_scan_id], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if blocked != 0 {
            reasons.push(code.to_owned());
        }
    }
    Ok(reasons)
}

impl SqliteScanStore {
    pub fn archive_history(
        &self,
        kind: &str,
        id: &str,
        archive_directory: Option<&Path>,
    ) -> Result<HistoryArchiveResult, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        // The immediate transaction is the maintenance fence. A mutation that
        // already owns a lease is observed below; a later mutation cannot
        // create its attempt until the archive snapshot is committed as
        // read-only. This prevents a cross-process writer from entering the
        // workflow graph between the protection check and the final export
        // query.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let root_scan_id = root_scan_id_for(&transaction, kind, id)?;
        let archive_id = Uuid::new_v4().to_string();
        let started_at = now();
        let reasons = archive_block_reasons(&transaction, &root_scan_id)?;
        if !reasons.is_empty() {
            let error = format!("history_archive_blocked:{}", reasons.join(","));
            insert_workflow_attempt(
                &transaction,
                &archive_id,
                &root_scan_id,
                &root_scan_id,
                "archive",
                Some(&root_scan_id),
                LOCAL_ATTEMPT_ACTOR,
                started_at,
            )?;
            finish_workflow_attempt(
                &transaction,
                &archive_id,
                "archive",
                RunStatus::Failed,
                now(),
                0,
                0,
                1,
                Some(&error),
            )?;
            transaction.commit().map_err(|error| error.to_string())?;
            return Err(error);
        }

        if let Some(existing) = completed_archive(&transaction, &root_scan_id)? {
            insert_workflow_attempt(
                &transaction,
                &archive_id,
                &root_scan_id,
                &root_scan_id,
                "archive",
                Some(&root_scan_id),
                LOCAL_ATTEMPT_ACTOR,
                started_at,
            )?;
            let verification = verify_archive_file(
                &existing.archive_path,
                &existing.sha256,
                existing.byte_count,
            );
            if let Err(error) = verification {
                finish_workflow_attempt(
                    &transaction,
                    &archive_id,
                    "archive",
                    RunStatus::Failed,
                    now(),
                    0,
                    0,
                    1,
                    Some(&error),
                )?;
                transaction.commit().map_err(|error| error.to_string())?;
                return Err(error);
            }
            finish_workflow_attempt(
                &transaction,
                &archive_id,
                "archive",
                RunStatus::Completed,
                now(),
                existing.record_count,
                0,
                0,
                None,
            )?;
            transaction.commit().map_err(|error| error.to_string())?;
            return Ok(existing);
        }

        insert_workflow_attempt(
            &transaction,
            &archive_id,
            &root_scan_id,
            &root_scan_id,
            "archive",
            Some(&root_scan_id),
            LOCAL_ATTEMPT_ACTOR,
            started_at,
        )?;
        let archive_dir = archive_directory
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_archive_directory(&self.database_path));
        fs::create_dir_all(&archive_dir).map_err(|error| error.to_string())?;
        let safe_root = safe_file_component(&root_scan_id)?;
        let final_path = archive_dir.join(format!("history-{safe_root}-{archive_id}.jsonl"));
        let temporary_path = archive_dir.join(format!(".history-{archive_id}.tmp"));
        let final_display = final_path.to_string_lossy().into_owned();
        let (final_encoding, final_raw) = encode_path(&final_path);
        transaction
            .execute(
                "INSERT INTO archive_manifests(
                     id,root_scan_id,schema_version,archive_path,
                     archive_path_encoding,archive_path_blob,item_count,sha256,status,
                     created_at,digest_algorithm,byte_count
                 ) VALUES(?1,?2,?3,?4,?5,?6,0,'','building',?7,'sha256',0)",
                params![
                    archive_id,
                    root_scan_id,
                    i64::from(ARCHIVE_SCHEMA_VERSION),
                    final_display,
                    final_encoding,
                    final_raw,
                    now()
                ],
            )
            .map_err(|error| error.to_string())?;

        let archive_result = (|| {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(|error| error.to_string())?;
            let mut sink = ArchiveSink::new(file);
            let database_uuid: String = transaction
                .query_row(
                    "SELECT database_uuid FROM application_metadata WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            sink.write_value(
                None,
                &json!({
                    "archive_schema_version": ARCHIVE_SCHEMA_VERSION,
                    "archive_id": archive_id,
                    "database_uuid": database_uuid,
                    "root_scan_id": root_scan_id,
                }),
            )?;
            export_workflow(&transaction, &root_scan_id, &mut sink)?;
            let (sha256, record_count, byte_count, counts) = sink.finish()?;
            publish_no_replace(&temporary_path, &final_path)?;
            verify_archive_file(&final_path, &sha256, byte_count)?;
            Ok::<_, String>((sha256, record_count, byte_count, counts))
        })();

        let (sha256, record_count, byte_count, counts) = match archive_result {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_file(&temporary_path);
                let _ = transaction.rollback();
                let failed_transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|database_error| database_error.to_string())?;
                insert_workflow_attempt(
                    &failed_transaction,
                    &archive_id,
                    &root_scan_id,
                    &root_scan_id,
                    "archive",
                    Some(&root_scan_id),
                    LOCAL_ATTEMPT_ACTOR,
                    started_at,
                )?;
                failed_transaction
                    .execute(
                        "INSERT INTO archive_manifests(
                         id,root_scan_id,schema_version,archive_path,
                         archive_path_encoding,archive_path_blob,item_count,sha256,
                         status,created_at,completed_at,digest_algorithm,byte_count
                     ) VALUES(?1,?2,?3,?4,?5,?6,0,'','failed',?7,?7,'sha256',0)",
                        params![
                            archive_id,
                            root_scan_id,
                            i64::from(ARCHIVE_SCHEMA_VERSION),
                            final_display,
                            final_encoding,
                            final_raw,
                            now()
                        ],
                    )
                    .map_err(|database_error| database_error.to_string())?;
                finish_workflow_attempt(
                    &failed_transaction,
                    &archive_id,
                    "archive",
                    RunStatus::Failed,
                    now(),
                    0,
                    0,
                    1,
                    Some(&error),
                )?;
                failed_transaction
                    .commit()
                    .map_err(|database_error| database_error.to_string())?;
                return Err(format!("history_archive_failed:{error}"));
            }
        };

        let completed_at = now();
        transaction
            .execute(
                "UPDATE archive_manifests SET item_count=?2,sha256=?3,status='completed',completed_at=?4,verified_at=?4,byte_count=?5 WHERE id=?1 AND status='building'",
                params![archive_id, record_count as i64, sha256, completed_at, byte_count as i64],
            )
            .map_err(|error| error.to_string())?;
        finish_workflow_attempt(
            &transaction,
            &archive_id,
            "archive",
            RunStatus::Completed,
            completed_at,
            record_count,
            0,
            0,
            None,
        )?;
        for (entity, count) in counts {
            transaction
                .execute(
                    "INSERT INTO archive_members(archive_id,entity_kind,entity_id,record_count) VALUES(?1,?2,?3,?4)",
                    params![archive_id, entity, root_scan_id, count as i64],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute(
                "UPDATE scan_runs SET archived_at=?2 WHERE id=?1",
                params![root_scan_id, completed_at],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE plan_runs SET archived_at=?2 WHERE scan_id=?1",
                params![root_scan_id, completed_at],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE execution_runs SET archived_at=?2 WHERE plan_id IN (SELECT id FROM plan_runs WHERE scan_id=?1)",
                params![root_scan_id, completed_at],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE verify_runs SET archived_at=?2 WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plan_runs WHERE scan_id=?1))",
                params![root_scan_id, completed_at],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE rollback_runs SET archived_at=?2 WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plan_runs WHERE scan_id=?1))",
                params![root_scan_id, completed_at],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;

        Ok(HistoryArchiveResult {
            archive_id,
            root_scan_id,
            archive_path: final_path,
            schema_version: ARCHIVE_SCHEMA_VERSION,
            record_count,
            byte_count,
            digest_algorithm: "sha256".into(),
            sha256,
            verified: true,
            already_archived: false,
        })
    }
}

fn completed_archive(
    connection: &Connection,
    root_scan_id: &str,
) -> Result<Option<HistoryArchiveResult>, String> {
    let newest_schema: Option<i64> = connection
        .query_row(
            "SELECT MAX(schema_version) FROM archive_manifests
              WHERE root_scan_id=?1 AND status='completed'",
            params![root_scan_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if newest_schema.is_some_and(|version| version > i64::from(ARCHIVE_SCHEMA_VERSION)) {
        return Err(format!(
            "archive_schema_too_new:{}>{ARCHIVE_SCHEMA_VERSION}",
            newest_schema.unwrap_or_default()
        ));
    }
    type CompletedArchiveRow = (String, String, Vec<u8>, i64, i64, i64, String, String);
    let row: Option<CompletedArchiveRow> = connection
        .query_row(
            "SELECT id,archive_path_encoding,archive_path_blob,schema_version,
                    item_count,byte_count,digest_algorithm,sha256
               FROM archive_manifests
              WHERE root_scan_id=?1 AND status='completed' AND schema_version=?2
              ORDER BY completed_at DESC,id DESC LIMIT 1",
            params![root_scan_id, i64::from(ARCHIVE_SCHEMA_VERSION)],
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
    row.map(
        |(
            archive_id,
            encoding,
            raw,
            schema_version,
            record_count,
            byte_count,
            digest_algorithm,
            sha256,
        )| {
            Ok(HistoryArchiveResult {
                archive_id,
                root_scan_id: root_scan_id.to_owned(),
                archive_path: decode_path(&encoding, &raw)?,
                schema_version: u32::try_from(schema_version)
                    .map_err(|_| "archive_schema_version_invalid".to_string())?,
                record_count: u64::try_from(record_count)
                    .map_err(|_| "archive_record_count_invalid".to_string())?,
                byte_count: u64::try_from(byte_count)
                    .map_err(|_| "archive_byte_count_invalid".to_string())?,
                digest_algorithm,
                sha256,
                verified: true,
                already_archived: true,
            })
        },
    )
    .transpose()
}

pub(super) fn verify_completed_archive(
    connection: &Connection,
    root_scan_id: &str,
) -> Result<HistoryArchiveResult, String> {
    let archive = completed_archive(connection, root_scan_id)?
        .ok_or_else(|| "verified_history_archive_required".to_string())?;
    verify_archive_file(&archive.archive_path, &archive.sha256, archive.byte_count)?;
    Ok(archive)
}

fn export_workflow(
    connection: &Connection,
    root_scan_id: &str,
    sink: &mut ArchiveSink,
) -> Result<(), String> {
    let queries = [
        ("scan_runs", "SELECT * FROM scan_runs WHERE id=?1 ORDER BY id"),
        ("workflow_attempts", "SELECT * FROM workflow_attempts WHERE root_scan_id=?1 ORDER BY started_at,id"),
        ("preflight_runs", "SELECT run.* FROM preflight_runs run JOIN workflow_attempts attempt ON attempt.id=run.parent_attempt_id WHERE attempt.root_scan_id=?1 ORDER BY run.started_at,run.id"),
        ("preflight_logs", "SELECT log.* FROM preflight_logs log JOIN preflight_runs run ON run.id=log.preflight_id JOIN workflow_attempts attempt ON attempt.id=run.parent_attempt_id WHERE attempt.root_scan_id=?1 ORDER BY run.started_at,run.id,log.ordinal"),
        ("scan_items", "SELECT * FROM scan_items WHERE scan_id=?1 ORDER BY path_encoding,path_blob"),
        ("scan_warnings", "SELECT * FROM scan_warnings WHERE scan_id=?1 ORDER BY created_at,id"),
        ("plan_runs", "SELECT * FROM plan_runs WHERE scan_id=?1 ORDER BY started_at,id"),
        ("plan_builds", "SELECT build.* FROM plan_builds build JOIN plan_runs plan ON plan.id=build.plan_id WHERE plan.scan_id=?1 ORDER BY build.created_at,build.id"),
        ("plan_item_staging", "SELECT stage.* FROM plan_item_staging stage JOIN plan_builds build ON build.id=stage.build_id JOIN plan_runs plan ON plan.id=build.plan_id WHERE plan.scan_id=?1 ORDER BY stage.build_id,stage.ordinal"),
        ("plan_music_anchor_staging", "SELECT stage.* FROM plan_music_anchor_staging stage JOIN plan_builds build ON build.id=stage.build_id JOIN plan_runs plan ON plan.id=build.plan_id WHERE plan.scan_id=?1 ORDER BY stage.build_id,stage.source_directory_encoding,stage.source_directory_blob,stage.music_item_id"),
        ("plan_target_allocations", "SELECT allocation.* FROM plan_target_allocations allocation JOIN plan_builds build ON build.id=allocation.build_id JOIN plan_runs plan ON plan.id=build.plan_id WHERE plan.scan_id=?1 ORDER BY allocation.build_id,allocation.normalized_target_key"),
        ("plan_items", "SELECT item.* FROM plan_items item JOIN plan_runs plan ON plan.id=item.plan_id WHERE plan.scan_id=?1 ORDER BY item.plan_id,item.ordinal,item.id"),
        ("plan_revision_changes", "SELECT change.* FROM plan_revision_changes change JOIN plan_runs plan ON plan.id=change.child_plan_id WHERE plan.scan_id=?1 ORDER BY change.child_plan_id,change.parent_plan_item_id"),
        ("plan_conflict_groups", "SELECT conflict.* FROM plan_conflict_groups conflict JOIN plan_runs plan ON plan.id=conflict.plan_id WHERE plan.scan_id=?1 ORDER BY conflict.plan_id,conflict.id"),
        ("plan_conflict_members", "SELECT member.* FROM plan_conflict_members member JOIN plan_conflict_groups conflict ON conflict.id=member.conflict_group_id JOIN plan_runs plan ON plan.id=conflict.plan_id WHERE plan.scan_id=?1 ORDER BY member.conflict_group_id,member.plan_item_id"),
        ("plan_conflict_candidates", "SELECT candidate.* FROM plan_conflict_candidates candidate JOIN plan_conflict_groups conflict ON conflict.id=candidate.conflict_group_id JOIN plan_runs plan ON plan.id=conflict.plan_id WHERE plan.scan_id=?1 ORDER BY candidate.conflict_group_id,candidate.ordinal"),
        ("plan_conflict_candidate_members", "SELECT member.* FROM plan_conflict_candidate_members member JOIN plan_conflict_groups conflict ON conflict.id=member.conflict_group_id JOIN plan_runs plan ON plan.id=conflict.plan_id WHERE plan.scan_id=?1 ORDER BY member.conflict_group_id,member.candidate_ordinal,member.plan_item_id"),
        ("execution_runs", "SELECT execution.* FROM execution_runs execution JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 ORDER BY execution.started_at,execution.id"),
        ("operation_logs", "SELECT log.* FROM operation_logs log JOIN execution_runs execution ON execution.id=log.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 ORDER BY log.execution_id,log.sequence_no,log.id"),
        ("verify_runs", "SELECT verify.* FROM verify_runs verify JOIN execution_runs execution ON execution.id=verify.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 ORDER BY verify.started_at,verify.id"),
        ("verify_logs", "SELECT log.* FROM verify_logs log JOIN verify_runs verify ON verify.id=log.verify_run_id JOIN execution_runs execution ON execution.id=verify.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 ORDER BY log.verify_run_id,log.created_at,log.id"),
        ("rollback_runs", "SELECT rollback.* FROM rollback_runs rollback JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 ORDER BY rollback.started_at,rollback.id"),
        ("rollback_logs", "SELECT log.* FROM rollback_logs log JOIN rollback_runs rollback ON rollback.id=log.rollback_run_id JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 ORDER BY log.rollback_run_id,log.created_at,log.id"),
        ("operation_journal", "SELECT journal.* FROM operation_journal journal WHERE journal.attempt_id IN (SELECT execution.id FROM execution_runs execution JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 UNION SELECT rollback.id FROM rollback_runs rollback JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1) ORDER BY journal.attempt_id,journal.sequence_no,journal.id"),
        ("operation_journal_events", "SELECT event.* FROM operation_journal_events event JOIN operation_journal journal ON journal.id=event.operation_id JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id WHERE attempt.root_scan_id=?1 ORDER BY journal.attempt_id,journal.sequence_no,event.transition_no"),
        ("recovery_runs", "SELECT recovery.* FROM recovery_runs recovery JOIN operation_journal journal ON journal.id=recovery.operation_id WHERE journal.attempt_id IN (SELECT execution.id FROM execution_runs execution JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1 UNION SELECT rollback.id FROM rollback_runs rollback JOIN execution_runs execution ON execution.id=rollback.execution_id JOIN plan_runs plan ON plan.id=execution.plan_id WHERE plan.scan_id=?1) ORDER BY recovery.started_at,recovery.id"),
        ("run_metrics", "SELECT metrics.* FROM run_metrics metrics WHERE metrics.run_id=?1 OR metrics.run_id IN (SELECT id FROM plan_runs WHERE scan_id=?1) OR metrics.run_id IN (SELECT id FROM workflow_attempts WHERE root_scan_id=?1) ORDER BY metrics.run_id,metrics.rowid"),
        ("diagnostic_events", "SELECT event.* FROM diagnostic_events event WHERE event.run_id=?1 OR event.run_id IN (SELECT id FROM plan_runs WHERE scan_id=?1) OR event.attempt_id IN (SELECT id FROM workflow_attempts WHERE root_scan_id=?1) ORDER BY event.created_at,event.id"),
    ];
    for (entity, query) in queries {
        export_query(connection, sink, entity, query, root_scan_id)?;
    }
    Ok(())
}

fn export_query(
    connection: &Connection,
    sink: &mut ArchiveSink,
    entity: &str,
    sql: &str,
    root_scan_id: &str,
) -> Result<(), String> {
    let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut rows = statement
        .query(params![root_scan_id])
        .map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let mut fields = Map::new();
        for (index, name) in columns.iter().enumerate() {
            fields.insert(
                name.clone(),
                sqlite_value(row.get_ref(index).map_err(|e| e.to_string())?)?,
            );
        }
        sink.write_value(Some(entity), &json!({"entity": entity, "fields": fields}))?;
    }
    Ok(())
}

fn sqlite_value(value: ValueRef<'_>) -> Result<Value, String> {
    match value {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(value) => Ok(Value::Number(value.into())),
        ValueRef::Real(value) => Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| "archive_non_finite_real".to_string()),
        ValueRef::Text(value) => std::str::from_utf8(value)
            .map(|text| Value::String(text.to_owned()))
            .map_err(|_| "archive_invalid_sqlite_text".to_string()),
        ValueRef::Blob(value) => Ok(json!({"base64": base64(value)})),
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn default_archive_directory(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("history-archives")
}

fn safe_file_component(value: &str) -> Result<String, String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("archive_root_id_not_file_safe".into());
    }
    Ok(value.to_owned())
}

fn publish_no_replace(temporary: &Path, final_path: &Path) -> Result<(), String> {
    fs::hard_link(temporary, final_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            "archive_target_exists".to_string()
        } else {
            error.to_string()
        }
    })?;
    fs::remove_file(temporary).map_err(|error| error.to_string())?;
    if let Some(parent) = final_path.parent() {
        crate::windows_fs::sync_directory(parent)
            .map_err(|error| format!("archive_directory_flush_failed:{error}"))?;
    }
    Ok(())
}

fn verify_archive_file(
    path: &Path,
    expected_sha256: &str,
    expected_bytes: u64,
) -> Result<(), String> {
    let file = File::open(path).map_err(|error| format!("archive_missing:{error}"))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| "archive_byte_count_overflow".to_string())?;
    }
    let actual_sha256 = format!("{:x}", digest.finalize());
    if total != expected_bytes || actual_sha256 != expected_sha256 {
        return Err("archive_digest_mismatch".into());
    }
    Ok(())
}
