use crate::path_codec::decode_path;
use music_folder_core::{windows_path_key, WINDOWS_PATH_KEY_VERSION};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

const LATEST_SCHEMA_VERSION: i64 = 16;

fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool, String> {
    connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| error.to_string())
        .map(|columns| columns.iter().any(|name| name == column))
}

fn has_index(connection: &Connection, index: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1",
            params![index],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(|error| error.to_string())
}

fn add_column_if_missing(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), String> {
    if !has_column(transaction, table, column)? {
        transaction
            .execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition};"
            ))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn migrate_v1(transaction: &Transaction<'_>) -> Result<(), String> {
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS scan_runs (
                 id TEXT PRIMARY KEY,
                 source_root TEXT NOT NULL,
                 status TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 warning_count INTEGER NOT NULL DEFAULT 0 CHECK(warning_count >= 0)
             );
             CREATE TABLE IF NOT EXISTS library_files (
                 path TEXT PRIMARY KEY,
                 size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                 mtime_ns TEXT NOT NULL,
                 metadata_json TEXT,
                 metadata_status TEXT NOT NULL,
                 last_seen_scan_id TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS scan_items (
                 scan_id TEXT NOT NULL REFERENCES scan_runs(id),
                 path TEXT NOT NULL,
                 PRIMARY KEY(scan_id,path)
             );
             CREATE TABLE IF NOT EXISTS scan_warnings (
                 id TEXT PRIMARY KEY,
                 scan_id TEXT NOT NULL REFERENCES scan_runs(id),
                 warning TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS plan_runs (
                 id TEXT PRIMARY KEY,
                 scan_id TEXT NOT NULL REFERENCES scan_runs(id),
                 target_root TEXT NOT NULL,
                 status TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 conflict_count INTEGER NOT NULL DEFAULT 0 CHECK(conflict_count >= 0),
                 risk_count INTEGER NOT NULL DEFAULT 0 CHECK(risk_count >= 0)
             );
             CREATE TABLE IF NOT EXISTS plan_items (
                 id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL REFERENCES plan_runs(id),
                 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                 source_path TEXT NOT NULL,
                 target_path TEXT,
                 action TEXT NOT NULL CHECK(action IN ('move','skip')),
                 risk TEXT NOT NULL,
                 reason TEXT
             );
             CREATE TABLE IF NOT EXISTS plan_conflict_groups (
                 id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL REFERENCES plan_runs(id) ON DELETE CASCADE,
                 kind TEXT NOT NULL,
                 normalized_target_path TEXT NOT NULL,
                 target_path TEXT NOT NULL,
                 existing_target_path TEXT
             );
             CREATE TABLE IF NOT EXISTS plan_conflict_members (
                 conflict_group_id TEXT NOT NULL
                     REFERENCES plan_conflict_groups(id) ON DELETE CASCADE,
                 plan_item_id TEXT NOT NULL REFERENCES plan_items(id) ON DELETE CASCADE,
                 PRIMARY KEY(conflict_group_id,plan_item_id)
             );
             CREATE TABLE IF NOT EXISTS plan_conflict_candidates (
                 conflict_group_id TEXT NOT NULL
                     REFERENCES plan_conflict_groups(id) ON DELETE CASCADE,
                 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                 target_path TEXT NOT NULL,
                 PRIMARY KEY(conflict_group_id,ordinal)
             );
             CREATE TABLE IF NOT EXISTS plan_conflict_candidate_members (
                 conflict_group_id TEXT NOT NULL,
                 candidate_ordinal INTEGER NOT NULL CHECK(candidate_ordinal >= 0),
                 plan_item_id TEXT NOT NULL,
                 PRIMARY KEY(conflict_group_id,candidate_ordinal,plan_item_id),
                 FOREIGN KEY(conflict_group_id,candidate_ordinal)
                     REFERENCES plan_conflict_candidates(conflict_group_id,ordinal)
                     ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS execution_runs (
                 id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL REFERENCES plan_runs(id),
                 mode TEXT NOT NULL CHECK(mode IN ('dry_run','apply')),
                 status TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 success_count INTEGER NOT NULL DEFAULT 0 CHECK(success_count >= 0),
                 skipped_count INTEGER NOT NULL DEFAULT 0 CHECK(skipped_count >= 0),
                 failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0)
             );
             CREATE TABLE IF NOT EXISTS operation_logs (
                 id TEXT PRIMARY KEY,
                 execution_id TEXT NOT NULL REFERENCES execution_runs(id),
                 plan_item_id TEXT NOT NULL REFERENCES plan_items(id),
                 sequence_no INTEGER NOT NULL CHECK(sequence_no >= 0),
                 source_path TEXT NOT NULL,
                 target_path TEXT,
                 action TEXT NOT NULL,
                 result TEXT NOT NULL,
                 error TEXT,
                 source_deleted INTEGER NOT NULL CHECK(source_deleted IN (0,1)),
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS verify_logs (
                 id TEXT PRIMARY KEY,
                 execution_id TEXT NOT NULL REFERENCES execution_runs(id),
                 operation_id TEXT NOT NULL REFERENCES operation_logs(id),
                 result TEXT NOT NULL,
                 error TEXT,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS rollback_logs (
                 id TEXT PRIMARY KEY,
                 execution_id TEXT NOT NULL REFERENCES execution_runs(id),
                 operation_id TEXT NOT NULL REFERENCES operation_logs(id),
                 result TEXT NOT NULL,
                 error TEXT,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS verify_runs (
                 id TEXT PRIMARY KEY,
                 execution_id TEXT NOT NULL REFERENCES execution_runs(id),
                 status TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 success_count INTEGER NOT NULL DEFAULT 0 CHECK(success_count >= 0),
                 failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0)
             );
             CREATE TABLE IF NOT EXISTS rollback_runs (
                 id TEXT PRIMARY KEY,
                 execution_id TEXT NOT NULL REFERENCES execution_runs(id),
                 mode TEXT NOT NULL CHECK(mode IN ('dry_run','rollback')),
                 status TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 success_count INTEGER NOT NULL DEFAULT 0 CHECK(success_count >= 0),
                 skipped_count INTEGER NOT NULL DEFAULT 0 CHECK(skipped_count >= 0),
                 failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0)
             );
             CREATE TABLE IF NOT EXISTS run_metrics (
                 run_id TEXT NOT NULL,
                 phase TEXT NOT NULL,
                 elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0),
                 item_count INTEGER NOT NULL DEFAULT 0 CHECK(item_count >= 0)
             );",
        )
        .map_err(|error| error.to_string())?;
    add_column_if_missing(transaction, "plan_runs", "snapshot_hash", "TEXT")?;
    add_column_if_missing(
        transaction,
        "operation_logs",
        "expected_size",
        "INTEGER CHECK(expected_size IS NULL OR expected_size >= 0)",
    )?;
    add_column_if_missing(
        transaction,
        "library_files",
        "kind",
        "TEXT NOT NULL DEFAULT 'music' CHECK(kind IN ('music','image'))",
    )
}

fn migrate_v2(transaction: &Transaction<'_>) -> Result<(), String> {
    for (table, column, definition) in [
        ("scan_runs", "snapshot_hash", "TEXT"),
        (
            "scan_runs",
            "snapshot_version",
            "INTEGER NOT NULL DEFAULT 1 CHECK(snapshot_version > 0)",
        ),
        ("scan_runs", "source_root_encoding", "TEXT"),
        ("scan_runs", "source_root_blob", "BLOB"),
        ("plan_runs", "parent_plan_id", "TEXT"),
        ("plan_runs", "rules_json", "TEXT"),
        (
            "plan_items",
            "target_origin",
            "TEXT NOT NULL DEFAULT 'rule' CHECK(target_origin IN ('rule','manual'))",
        ),
        ("plan_items", "conflict_group_id", "TEXT"),
    ] {
        add_column_if_missing(transaction, table, column, definition)?;
    }
    Ok(())
}

fn migrate_v3(transaction: &Transaction<'_>) -> Result<(), String> {
    transaction
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_scan_items_scan ON scan_items(scan_id);
             CREATE INDEX IF NOT EXISTS idx_plan_items_plan ON plan_items(plan_id,ordinal);
             CREATE INDEX IF NOT EXISTS idx_plan_conflict_groups_plan_target
                 ON plan_conflict_groups(plan_id,normalized_target_path);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_operation_logs_execution_sequence
                 ON operation_logs(execution_id,sequence_no);",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v4(transaction: &Transaction<'_>, applied_at: i64) -> Result<(), String> {
    let has_snapshot_columns = [
        "size_bytes",
        "mtime_ns",
        "metadata_json",
        "metadata_status",
        "kind",
    ]
    .into_iter()
    .map(|column| has_column(transaction, "scan_items", column))
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .all(|present| present);

    if !has_snapshot_columns {
        let missing_cache_rows: i64 = transaction
            .query_row(
                "SELECT COUNT(*)
                   FROM scan_items snapshot
                   LEFT JOIN library_files cache ON cache.path=snapshot.path
                  WHERE cache.path IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if missing_cache_rows != 0 {
            return Err(format!(
                "scan_snapshot_backfill_missing_library_files:{missing_cache_rows}"
            ));
        }
    }

    transaction
        .execute_batch(
            "DROP TABLE IF EXISTS scan_items_v4_new;
             CREATE TABLE scan_items_v4_new (
                 scan_id TEXT NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
                 path TEXT NOT NULL,
                 size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                 mtime_ns TEXT NOT NULL CHECK(length(mtime_ns) > 0),
                 metadata_json TEXT,
                 metadata_status TEXT NOT NULL
                     CHECK(metadata_status IN ('ok','error','not_applicable')),
                 kind TEXT NOT NULL CHECK(kind IN ('music','image')),
                 CHECK(
                     (metadata_status='ok' AND metadata_json IS NOT NULL) OR
                     (metadata_status!='ok' AND metadata_json IS NULL)
                 ),
                 PRIMARY KEY(scan_id,path)
             );",
        )
        .map_err(|error| error.to_string())?;

    if has_snapshot_columns {
        transaction
            .execute_batch(
                "INSERT INTO scan_items_v4_new(
                     scan_id,path,size_bytes,mtime_ns,metadata_json,metadata_status,kind
                 )
                 SELECT scan_id,path,size_bytes,mtime_ns,metadata_json,
                        CASE
                            WHEN metadata_json IS NOT NULL THEN 'ok'
                            WHEN kind='image' THEN 'not_applicable'
                            ELSE 'error'
                        END,
                        CASE WHEN kind='image' THEN 'image' ELSE 'music' END
                   FROM scan_items;",
            )
            .map_err(|error| error.to_string())?;
    } else {
        transaction
            .execute_batch(
                "INSERT INTO scan_items_v4_new(
                     scan_id,path,size_bytes,mtime_ns,metadata_json,metadata_status,kind
                 )
                 SELECT snapshot.scan_id,snapshot.path,cache.size_bytes,cache.mtime_ns,
                        cache.metadata_json,
                        CASE
                            WHEN cache.metadata_json IS NOT NULL THEN 'ok'
                            WHEN cache.kind='image' THEN 'not_applicable'
                            ELSE 'error'
                        END,
                        CASE WHEN cache.kind='image' THEN 'image' ELSE 'music' END
                   FROM scan_items snapshot
                   JOIN library_files cache ON cache.path=snapshot.path;",
            )
            .map_err(|error| error.to_string())?;
    }

    transaction
        .execute_batch(
            "DROP TABLE scan_items;
             ALTER TABLE scan_items_v4_new RENAME TO scan_items;
             CREATE INDEX idx_scan_items_scan ON scan_items(scan_id);
             CREATE TRIGGER scan_items_no_update
             BEFORE UPDATE ON scan_items
             BEGIN
                 SELECT RAISE(ABORT,'scan_items_immutable');
             END;
             CREATE TRIGGER scan_items_only_while_running
             BEFORE INSERT ON scan_items
             WHEN COALESCE((SELECT status FROM scan_runs WHERE id=NEW.scan_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'scan_not_running');
             END;
             CREATE TABLE IF NOT EXISTS metadata_cache_entries (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 path TEXT NOT NULL,
                 size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                 mtime_ns TEXT NOT NULL CHECK(length(mtime_ns) > 0),
                 reader_version TEXT NOT NULL CHECK(length(reader_version) > 0),
                 schema_version INTEGER NOT NULL CHECK(schema_version >= 0),
                 metadata_json TEXT,
                 metadata_status TEXT NOT NULL CHECK(metadata_status IN ('ok','error')),
                 kind TEXT NOT NULL CHECK(kind IN ('music','image')),
                 scan_id TEXT,
                 created_at INTEGER NOT NULL,
                 CHECK(
                     (metadata_status='ok' AND metadata_json IS NOT NULL) OR
                     (metadata_status='error' AND metadata_json IS NULL)
                 )
             );
             CREATE INDEX IF NOT EXISTS idx_metadata_cache_lookup
                 ON metadata_cache_entries(
                     path,size_bytes,mtime_ns,reader_version,schema_version,id DESC
                 );
             CREATE TRIGGER IF NOT EXISTS metadata_cache_entries_no_update
             BEFORE UPDATE ON metadata_cache_entries
             BEGIN
                 SELECT RAISE(ABORT,'metadata_cache_entries_append_only');
             END;",
        )
        .map_err(|error| error.to_string())?;

    transaction
        .execute(
            "INSERT INTO metadata_cache_entries(
                 path,size_bytes,mtime_ns,reader_version,schema_version,
                 metadata_json,metadata_status,kind,scan_id,created_at
             )
             SELECT path,size_bytes,mtime_ns,'legacy-unknown',0,metadata_json,
                    CASE WHEN metadata_json IS NOT NULL THEN 'ok' ELSE 'error' END,
                    CASE WHEN kind='image' THEN 'image' ELSE 'music' END,
                    last_seen_scan_id,?1
               FROM library_files",
            params![applied_at],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn migrate_v5(transaction: &Transaction<'_>) -> Result<(), String> {
    for (column, definition) in [
        (
            "source_size_bytes",
            "INTEGER CHECK(source_size_bytes IS NULL OR source_size_bytes >= 0)",
        ),
        (
            "source_mtime_ns",
            "TEXT CHECK(source_mtime_ns IS NULL OR length(source_mtime_ns) > 0)",
        ),
        ("source_file_identity", "BLOB"),
        (
            "source_identity_version",
            "INTEGER CHECK(source_identity_version IS NULL OR source_identity_version > 0)",
        ),
        ("source_content_hash", "BLOB"),
        (
            "source_hash_algorithm",
            "TEXT CHECK(source_hash_algorithm IS NULL OR length(source_hash_algorithm) > 0)",
        ),
        (
            "source_hash_version",
            "INTEGER CHECK(source_hash_version IS NULL OR source_hash_version > 0)",
        ),
    ] {
        add_column_if_missing(transaction, "plan_items", column, definition)?;
    }

    transaction
        .execute_batch(
            "UPDATE plan_items
                SET source_size_bytes=(
                        SELECT snapshot.size_bytes
                          FROM plan_runs plan
                          JOIN scan_items snapshot
                            ON snapshot.scan_id=plan.scan_id
                           AND snapshot.path=plan_items.source_path
                         WHERE plan.id=plan_items.plan_id
                    ),
                    source_mtime_ns=(
                        SELECT snapshot.mtime_ns
                          FROM plan_runs plan
                          JOIN scan_items snapshot
                            ON snapshot.scan_id=plan.scan_id
                           AND snapshot.path=plan_items.source_path
                         WHERE plan.id=plan_items.plan_id
                    )
              WHERE source_size_bytes IS NULL OR source_mtime_ns IS NULL;
             CREATE INDEX IF NOT EXISTS idx_plan_items_source_expectation
                 ON plan_items(plan_id,source_path,source_size_bytes,source_mtime_ns);",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v6(transaction: &Transaction<'_>) -> Result<(), String> {
    for (table, column, definition) in [
        // Early v2 databases can record the migration before these lossless
        // root columns exist, so v6 also repairs that partial legacy state.
        ("scan_runs", "source_root_encoding", "TEXT"),
        ("scan_runs", "source_root_blob", "BLOB"),
        ("scan_items", "path_encoding", "TEXT"),
        ("scan_items", "path_blob", "BLOB"),
        ("scan_items", "content_sha256", "TEXT"),
        ("scan_items", "file_identity", "TEXT"),
        (
            "scan_items",
            "fingerprint_version",
            "INTEGER NOT NULL DEFAULT 1 CHECK(fingerprint_version > 0)",
        ),
        ("metadata_cache_entries", "path_encoding", "TEXT"),
        ("metadata_cache_entries", "path_blob", "BLOB"),
        ("metadata_cache_entries", "content_sha256", "TEXT"),
        ("metadata_cache_entries", "file_identity", "TEXT"),
        (
            "metadata_cache_entries",
            "fingerprint_version",
            "INTEGER NOT NULL DEFAULT 1 CHECK(fingerprint_version > 0)",
        ),
        ("plan_runs", "target_root_encoding", "TEXT"),
        ("plan_runs", "target_root_blob", "BLOB"),
        ("plan_runs", "scan_snapshot_hash", "TEXT"),
        (
            "plan_runs",
            "snapshot_version",
            "INTEGER NOT NULL DEFAULT 1 CHECK(snapshot_version > 0)",
        ),
        (
            "plan_runs",
            "path_policy_version",
            "INTEGER NOT NULL DEFAULT 1 CHECK(path_policy_version > 0)",
        ),
        (
            "plan_runs",
            "legal_hold",
            "INTEGER NOT NULL DEFAULT 0 CHECK(legal_hold IN (0,1))",
        ),
        ("plan_runs", "archived_at", "INTEGER"),
        ("plan_items", "source_path_encoding", "TEXT"),
        ("plan_items", "source_path_blob", "BLOB"),
        ("plan_items", "target_path_encoding", "TEXT"),
        ("plan_items", "target_path_blob", "BLOB"),
        ("operation_logs", "expected_content_sha256", "TEXT"),
        ("operation_logs", "expected_file_identity", "TEXT"),
        ("verify_runs", "subject_kind", "TEXT"),
        ("verify_runs", "subject_id", "TEXT"),
        ("verify_logs", "verify_run_id", "TEXT"),
        ("rollback_logs", "rollback_run_id", "TEXT"),
    ] {
        add_column_if_missing(transaction, table, column, definition)?;
    }

    transaction
        .execute_batch(
            "DROP TRIGGER IF EXISTS scan_items_no_update;
             DROP TRIGGER IF EXISTS metadata_cache_entries_no_update;
             UPDATE scan_runs
                SET source_root_encoding=COALESCE(source_root_encoding,'utf8_legacy_v1'),
                    source_root_blob=COALESCE(source_root_blob,CAST(source_root AS BLOB));
             UPDATE scan_items
                SET path_encoding=COALESCE(path_encoding,'utf8_legacy_v1'),
                    path_blob=COALESCE(path_blob,CAST(path AS BLOB))
              WHERE path_encoding IS NULL OR path_blob IS NULL;
             UPDATE metadata_cache_entries
                SET path_encoding=COALESCE(path_encoding,'utf8_legacy_v1'),
                    path_blob=COALESCE(path_blob,CAST(path AS BLOB))
              WHERE path_encoding IS NULL OR path_blob IS NULL;
             UPDATE plan_runs
                SET target_root_encoding=COALESCE(target_root_encoding,'utf8_legacy_v1'),
                    target_root_blob=COALESCE(target_root_blob,CAST(target_root AS BLOB));
             UPDATE plan_items
                SET source_path_encoding=COALESCE(source_path_encoding,'utf8_legacy_v1'),
                    source_path_blob=COALESCE(source_path_blob,CAST(source_path AS BLOB)),
                    target_path_encoding=CASE WHEN target_path IS NULL THEN NULL
                                              ELSE COALESCE(target_path_encoding,'utf8_legacy_v1') END,
                    target_path_blob=CASE WHEN target_path IS NULL THEN NULL
                                          ELSE COALESCE(target_path_blob,CAST(target_path AS BLOB)) END;
             UPDATE verify_runs
                SET subject_kind=COALESCE(subject_kind,'execution'),
                    subject_id=COALESCE(subject_id,execution_id);
             UPDATE verify_logs
                SET verify_run_id=COALESCE(
                    verify_run_id,
                    (SELECT id FROM verify_runs
                      WHERE verify_runs.execution_id=verify_logs.execution_id
                      ORDER BY started_at,id LIMIT 1)
                );
             UPDATE rollback_logs
                SET rollback_run_id=COALESCE(
                    rollback_run_id,
                    (SELECT id FROM rollback_runs
                      WHERE rollback_runs.execution_id=rollback_logs.execution_id
                      ORDER BY started_at,id LIMIT 1)
                );
             CREATE INDEX IF NOT EXISTS idx_scan_items_path_blob
                 ON scan_items(scan_id,path_encoding,path_blob);
             CREATE INDEX IF NOT EXISTS idx_plan_items_source_blob
                 ON plan_items(plan_id,source_path_encoding,source_path_blob);
             CREATE INDEX IF NOT EXISTS idx_verify_logs_attempt
                 ON verify_logs(verify_run_id,created_at,id);
             CREATE INDEX IF NOT EXISTS idx_rollback_logs_attempt
                 ON rollback_logs(rollback_run_id,created_at,id);
             CREATE TRIGGER scan_items_no_update
             BEFORE UPDATE ON scan_items
             BEGIN
                 SELECT RAISE(ABORT,'scan_items_immutable');
             END;
             CREATE TRIGGER metadata_cache_entries_no_update
             BEFORE UPDATE ON metadata_cache_entries
             BEGIN
                 SELECT RAISE(ABORT,'metadata_cache_entries_append_only');
             END;

             CREATE TABLE IF NOT EXISTS mutation_lease_sequence (
                 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                 last_fencing_token INTEGER NOT NULL CHECK(last_fencing_token >= 0)
             );
             INSERT OR IGNORE INTO mutation_lease_sequence(singleton,last_fencing_token)
                 VALUES(1,0);
             CREATE TABLE IF NOT EXISTS application_metadata (
                 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                 database_uuid TEXT NOT NULL UNIQUE,
                 created_at INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO application_metadata(singleton,database_uuid,created_at)
                 VALUES(1,lower(hex(randomblob(16))),strftime('%s','now'));
             CREATE TABLE IF NOT EXISTS mutation_leases (
                 scope_key TEXT PRIMARY KEY,
                 scope_path_encoding TEXT NOT NULL,
                 scope_path_blob BLOB NOT NULL,
                 owner_id TEXT NOT NULL,
                 fencing_token INTEGER NOT NULL UNIQUE CHECK(fencing_token > 0),
                 heartbeat_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 CHECK(expires_at > heartbeat_at)
             );

             CREATE TABLE IF NOT EXISTS operation_journal (
                 id TEXT PRIMARY KEY,
                 attempt_id TEXT NOT NULL,
                 direction TEXT NOT NULL CHECK(direction IN ('apply','rollback','recovery')),
                 plan_item_id TEXT NOT NULL,
                 sequence_no INTEGER NOT NULL CHECK(sequence_no >= 0),
                 state TEXT NOT NULL CHECK(state IN (
                     'prepared','staging','staged','content_verified','published',
                     'source_deleted','completed','failed','recovery_required'
                 )),
                 source_path TEXT NOT NULL,
                 source_path_encoding TEXT NOT NULL,
                 source_path_blob BLOB NOT NULL,
                 target_path TEXT NOT NULL,
                 target_path_encoding TEXT NOT NULL,
                 target_path_blob BLOB NOT NULL,
                 temporary_path TEXT NOT NULL,
                 temporary_path_encoding TEXT NOT NULL,
                 temporary_path_blob BLOB NOT NULL,
                 expected_size INTEGER CHECK(expected_size IS NULL OR expected_size >= 0),
                 expected_content_sha256 TEXT,
                 expected_file_identity TEXT,
                 staged_size INTEGER CHECK(staged_size IS NULL OR staged_size >= 0),
                 staged_content_sha256 TEXT,
                 fencing_token INTEGER NOT NULL CHECK(fencing_token >= 0),
                 error TEXT,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 UNIQUE(attempt_id,direction,sequence_no)
             );
             CREATE INDEX IF NOT EXISTS idx_operation_journal_recovery
                 ON operation_journal(state,updated_at,id);

             CREATE TABLE IF NOT EXISTS recovery_runs (
                 id TEXT PRIMARY KEY,
                 operation_id TEXT NOT NULL REFERENCES operation_journal(id),
                 mode TEXT NOT NULL CHECK(mode IN ('dry_run','recovery')),
                 status TEXT NOT NULL,
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 error TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_recovery_runs_operation
                 ON recovery_runs(operation_id,started_at,id);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_recovery_runs_active
                 ON recovery_runs(operation_id) WHERE status='running';

             CREATE TABLE IF NOT EXISTS diagnostic_events (
                 id TEXT PRIMARY KEY,
                 run_id TEXT,
                 attempt_id TEXT,
                 class TEXT NOT NULL CHECK(class IN ('progress','debug','diagnostic','audit','recovery')),
                 code TEXT NOT NULL,
                 payload_json TEXT,
                 contains_sensitive_path INTEGER NOT NULL DEFAULT 0
                     CHECK(contains_sensitive_path IN (0,1)),
                 protected INTEGER NOT NULL DEFAULT 0 CHECK(protected IN (0,1)),
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_diagnostic_retention
                 ON diagnostic_events(class,protected,created_at,id);

             CREATE TABLE IF NOT EXISTS archive_manifests (
                 id TEXT PRIMARY KEY,
                 root_scan_id TEXT NOT NULL,
                 schema_version INTEGER NOT NULL CHECK(schema_version > 0),
                 archive_path TEXT NOT NULL,
                 item_count INTEGER NOT NULL CHECK(item_count >= 0),
                 sha256 TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('building','completed','failed')),
                 created_at INTEGER NOT NULL,
                 completed_at INTEGER
             );

             CREATE TABLE IF NOT EXISTS plan_builds (
                 id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL,
                 owner_id TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('building','completed','failed','cancelled')),
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS plan_item_staging (
                 build_id TEXT NOT NULL REFERENCES plan_builds(id) ON DELETE CASCADE,
                 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                 source_path_encoding TEXT NOT NULL,
                 source_path_blob BLOB NOT NULL,
                 draft_json TEXT NOT NULL,
                 normalized_target_key TEXT,
                 PRIMARY KEY(build_id,ordinal)
             );
             CREATE INDEX IF NOT EXISTS idx_plan_staging_target
                 ON plan_item_staging(build_id,normalized_target_key,ordinal);",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v7(transaction: &Transaction<'_>) -> Result<(), String> {
    for (table, column, definition) in [
        (
            "scan_runs",
            "legal_hold",
            "INTEGER NOT NULL DEFAULT 0 CHECK(legal_hold IN (0,1))",
        ),
        ("scan_runs", "archived_at", "INTEGER"),
        ("execution_runs", "archived_at", "INTEGER"),
        ("verify_runs", "archived_at", "INTEGER"),
        ("rollback_runs", "archived_at", "INTEGER"),
        ("operation_logs", "source_path_encoding", "TEXT"),
        ("operation_logs", "source_path_blob", "BLOB"),
        ("operation_logs", "target_path_encoding", "TEXT"),
        ("operation_logs", "target_path_blob", "BLOB"),
        (
            "archive_manifests",
            "digest_algorithm",
            "TEXT NOT NULL DEFAULT 'sha256'",
        ),
        (
            "archive_manifests",
            "byte_count",
            "INTEGER NOT NULL DEFAULT 0 CHECK(byte_count >= 0)",
        ),
        ("archive_manifests", "verified_at", "INTEGER"),
        ("diagnostic_events", "severity", "TEXT"),
        ("diagnostic_events", "phase", "TEXT"),
        ("diagnostic_events", "item_id", "TEXT"),
        ("diagnostic_events", "sequence_no", "INTEGER"),
        ("diagnostic_events", "path_role", "TEXT"),
        (
            "diagnostic_events",
            "payload_schema_version",
            "INTEGER NOT NULL DEFAULT 1 CHECK(payload_schema_version > 0)",
        ),
        ("diagnostic_events", "message_key", "TEXT"),
    ] {
        add_column_if_missing(transaction, table, column, definition)?;
    }

    transaction
        .execute_batch(
            "UPDATE operation_logs
                SET source_path_encoding=COALESCE(source_path_encoding,'utf8_legacy_v1'),
                    source_path_blob=COALESCE(source_path_blob,CAST(source_path AS BLOB)),
                    target_path_encoding=CASE WHEN target_path IS NULL THEN NULL
                                              ELSE COALESCE(target_path_encoding,'utf8_legacy_v1') END,
                    target_path_blob=CASE WHEN target_path IS NULL THEN NULL
                                          ELSE COALESCE(target_path_blob,CAST(target_path AS BLOB)) END;
             CREATE INDEX IF NOT EXISTS idx_operation_logs_source_blob
                 ON operation_logs(execution_id,source_path_encoding,source_path_blob);
             CREATE INDEX IF NOT EXISTS idx_archive_manifests_root
                 ON archive_manifests(root_scan_id,status,completed_at,id);
             CREATE TABLE IF NOT EXISTS archive_members (
                 archive_id TEXT NOT NULL REFERENCES archive_manifests(id) ON DELETE CASCADE,
                 entity_kind TEXT NOT NULL,
                 entity_id TEXT NOT NULL,
                 record_count INTEGER NOT NULL CHECK(record_count >= 0),
                 PRIMARY KEY(archive_id,entity_kind,entity_id)
             );
             CREATE TABLE IF NOT EXISTS retention_runs (
                 id TEXT PRIMARY KEY,
                 policy_version INTEGER NOT NULL CHECK(policy_version > 0),
                 cutoff_by_class_json TEXT NOT NULL,
                 deleted_counts_json TEXT,
                 status TEXT NOT NULL CHECK(status IN ('running','completed','failed')),
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 error TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_retention_runs_started
                 ON retention_runs(started_at,id);",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v8(transaction: &Transaction<'_>) -> Result<(), String> {
    for (table, column, definition) in [
        ("plan_builds", "parent_plan_id", "TEXT"),
        ("plan_builds", "phase", "TEXT NOT NULL DEFAULT 'drafting'"),
        (
            "plan_builds",
            "cursor_ordinal",
            "INTEGER NOT NULL DEFAULT 0 CHECK(cursor_ordinal >= 0)",
        ),
        (
            "plan_builds",
            "peak_batch_items",
            "INTEGER NOT NULL DEFAULT 0 CHECK(peak_batch_items >= 0)",
        ),
        ("plan_builds", "failure_reason", "TEXT"),
        ("plan_item_staging", "target_path_encoding", "TEXT"),
        ("plan_item_staging", "target_path_blob", "BLOB"),
        ("plan_item_staging", "original_target_key", "TEXT"),
        ("plan_item_staging", "parent_plan_item_id", "TEXT"),
        ("plan_item_staging", "change_origin", "TEXT"),
        ("plan_item_staging", "change_reason", "TEXT"),
        ("plan_items", "source_kind", "TEXT"),
        ("plan_items", "source_metadata_json", "TEXT"),
    ] {
        add_column_if_missing(transaction, table, column, definition)?;
    }
    transaction
        .execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_builds_plan
                 ON plan_builds(plan_id);
             CREATE INDEX IF NOT EXISTS idx_plan_staging_original_target
                 ON plan_item_staging(build_id,original_target_key,ordinal);
             CREATE TABLE IF NOT EXISTS plan_music_anchor_staging (
                 build_id TEXT NOT NULL REFERENCES plan_builds(id) ON DELETE CASCADE,
                 source_directory_encoding TEXT NOT NULL,
                 source_directory_blob BLOB NOT NULL,
                 target_directory_encoding TEXT NOT NULL,
                 target_directory_blob BLOB NOT NULL,
                 disc_parent_encoding TEXT,
                 disc_parent_blob BLOB,
                 music_item_id TEXT NOT NULL,
                 PRIMARY KEY(
                     build_id,source_directory_encoding,source_directory_blob,
                     target_directory_encoding,target_directory_blob,music_item_id
                 )
             );
             CREATE INDEX IF NOT EXISTS idx_plan_anchor_source
                 ON plan_music_anchor_staging(
                     build_id,source_directory_encoding,source_directory_blob,music_item_id
                 );
             CREATE TABLE IF NOT EXISTS plan_target_allocations (
                 build_id TEXT NOT NULL REFERENCES plan_builds(id) ON DELETE CASCADE,
                 normalized_target_key TEXT NOT NULL,
                 PRIMARY KEY(build_id,normalized_target_key)
             );
             CREATE TABLE IF NOT EXISTS plan_revision_changes (
                 child_plan_id TEXT NOT NULL REFERENCES plan_runs(id) ON DELETE CASCADE,
                 parent_plan_item_id TEXT NOT NULL REFERENCES plan_items(id),
                 child_plan_item_id TEXT NOT NULL REFERENCES plan_items(id) ON DELETE CASCADE,
                 origin TEXT NOT NULL CHECK(origin IN ('rule','manual')),
                 reason TEXT NOT NULL,
                 PRIMARY KEY(child_plan_id,parent_plan_item_id)
             );",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v9(transaction: &Transaction<'_>) -> Result<(), String> {
    add_column_if_missing(
        transaction,
        "recovery_runs",
        "action",
        "TEXT NOT NULL DEFAULT 'manual_intervention'",
    )?;
    transaction
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_recovery_runs_action
                 ON recovery_runs(action,status,started_at,id);",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v10(transaction: &Transaction<'_>) -> Result<(), String> {
    add_column_if_missing(
        transaction,
        "operation_journal",
        "strategy",
        "TEXT NOT NULL DEFAULT 'copy_publish_delete' CHECK(strategy IN ('atomic_no_replace_rename','copy_publish_delete','conditional_delete_only'))",
    )?;
    add_column_if_missing(
        transaction,
        "operation_journal",
        "staged_file_identity",
        "TEXT",
    )
}

fn migrate_v11(transaction: &Transaction<'_>) -> Result<(), String> {
    add_column_if_missing(
        transaction,
        "operation_journal",
        "transition_no",
        "INTEGER NOT NULL DEFAULT 0 CHECK(transition_no >= 0)",
    )?;

    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_attempts (
                 id TEXT PRIMARY KEY,
                 root_scan_id TEXT NOT NULL,
                 logical_run_id TEXT NOT NULL,
                 kind TEXT NOT NULL CHECK(kind IN (
                     'scan','plan','plan_revision','dry_run','apply','verify',
                     'rollback_dry_run','rollback','recovery_dry_run','recovery',
                     'archive'
                 )),
                 attempt_no INTEGER NOT NULL CHECK(attempt_no > 0),
                 parent_attempt_id TEXT REFERENCES workflow_attempts(id)
                     DEFERRABLE INITIALLY DEFERRED,
                 predecessor_attempt_id TEXT REFERENCES workflow_attempts(id)
                     DEFERRABLE INITIALLY DEFERRED,
                 actor TEXT NOT NULL CHECK(length(actor) > 0),
                 status TEXT NOT NULL CHECK(status IN (
                     'running','completed','failed','cancelled','partial',
                     'recovery_required','archived'
                 )),
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 success_count INTEGER NOT NULL DEFAULT 0 CHECK(success_count >= 0),
                 skipped_count INTEGER NOT NULL DEFAULT 0 CHECK(skipped_count >= 0),
                 failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0),
                 error_code TEXT,
                 UNIQUE(logical_run_id,kind,attempt_no),
                 CHECK((status='running' AND finished_at IS NULL)
                       OR (status<>'running' AND finished_at IS NOT NULL))
             );
             CREATE INDEX IF NOT EXISTS idx_workflow_attempts_root
                 ON workflow_attempts(root_scan_id,started_at,id);
             CREATE INDEX IF NOT EXISTS idx_workflow_attempts_parent
                 ON workflow_attempts(parent_attempt_id,kind,attempt_no,id);

             CREATE TABLE IF NOT EXISTS operation_journal_events (
                 operation_id TEXT NOT NULL REFERENCES operation_journal(id)
                     ON DELETE CASCADE,
                 transition_no INTEGER NOT NULL CHECK(transition_no >= 0),
                 from_state TEXT CHECK(from_state IS NULL OR from_state IN (
                     'prepared','staging','staged','content_verified','published',
                     'source_deleted','completed','failed','recovery_required'
                 )),
                 to_state TEXT NOT NULL CHECK(to_state IN (
                     'prepared','staging','staged','content_verified','published',
                     'source_deleted','completed','failed','recovery_required'
                 )),
                 fencing_token INTEGER NOT NULL CHECK(fencing_token >= 0),
                 payload_schema_version INTEGER NOT NULL DEFAULT 1
                     CHECK(payload_schema_version > 0),
                 payload_json TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY(operation_id,transition_no)
             );
             CREATE INDEX IF NOT EXISTS idx_operation_journal_events_created
                 ON operation_journal_events(created_at,operation_id,transition_no);
             CREATE TRIGGER IF NOT EXISTS operation_journal_events_no_update
             BEFORE UPDATE ON operation_journal_events
             BEGIN
                 SELECT RAISE(ABORT,'operation_journal_events_append_only');
             END;",
        )
        .map_err(|error| error.to_string())?;

    // Legacy rows already represent one invocation each.  Backfill the
    // supertype in dependency order so parent/predecessor references are
    // complete when this transaction commits.
    transaction
        .execute_batch(
            "INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT id,id,id,'scan',1,NULL,NULL,'legacy_migration',
                    CASE status
                      WHEN 'running' THEN 'running' WHEN 'completed' THEN 'completed'
                      WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled'
                      WHEN 'partial' THEN 'partial'
                      WHEN 'recovery_required' THEN 'recovery_required'
                      WHEN 'archived' THEN 'archived' ELSE 'failed' END,
                    started_at,
                    CASE WHEN status='running' THEN NULL
                         ELSE COALESCE(finished_at,started_at) END,
                    (SELECT COUNT(*) FROM scan_items item WHERE item.scan_id=scan_runs.id),
                    0,warning_count,
                    CASE WHEN status IN ('running','completed','cancelled','partial',
                                         'recovery_required','archived') THEN NULL
                         WHEN status='failed' THEN 'legacy_scan_failed'
                         ELSE 'legacy_status_unknown' END
               FROM scan_runs;

             INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT id,scan_id,
                    CASE WHEN parent_plan_id IS NULL THEN scan_id ELSE parent_plan_id END,
                    CASE WHEN parent_plan_id IS NULL THEN 'plan' ELSE 'plan_revision' END,
                    ROW_NUMBER() OVER(
                        PARTITION BY CASE WHEN parent_plan_id IS NULL THEN scan_id
                                          ELSE parent_plan_id END,
                                     CASE WHEN parent_plan_id IS NULL THEN 'plan'
                                          ELSE 'plan_revision' END
                        ORDER BY started_at,id
                    ),
                    COALESCE(parent_plan_id,scan_id),NULL,'legacy_migration',
                    CASE status
                      WHEN 'running' THEN 'running' WHEN 'completed' THEN 'completed'
                      WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled'
                      WHEN 'partial' THEN 'partial'
                      WHEN 'recovery_required' THEN 'recovery_required'
                      WHEN 'archived' THEN 'archived' ELSE 'failed' END,
                    started_at,
                    CASE WHEN status='running' THEN NULL
                         ELSE COALESCE(finished_at,started_at) END,
                    (SELECT COUNT(*) FROM plan_items item WHERE item.plan_id=plan_runs.id),
                    conflict_count,risk_count,
                    CASE WHEN status='failed' THEN 'legacy_plan_failed'
                         WHEN status NOT IN ('running','completed','cancelled','partial',
                                             'recovery_required','archived')
                         THEN 'legacy_status_unknown' END
               FROM plan_runs;

             INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT execution.id,plan.scan_id,execution.plan_id,
                    CASE execution.mode WHEN 'dry_run' THEN 'dry_run' ELSE 'apply' END,
                    ROW_NUMBER() OVER(
                        PARTITION BY execution.plan_id,execution.mode
                        ORDER BY execution.started_at,execution.id
                    ),
                    execution.plan_id,NULL,'legacy_migration',
                    CASE execution.status
                      WHEN 'running' THEN 'running' WHEN 'completed' THEN 'completed'
                      WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled'
                      WHEN 'partial' THEN 'partial'
                      WHEN 'recovery_required' THEN 'recovery_required'
                      WHEN 'archived' THEN 'archived' ELSE 'failed' END,
                    execution.started_at,
                    CASE WHEN execution.status='running' THEN NULL
                         ELSE COALESCE(execution.finished_at,execution.started_at) END,
                    execution.success_count,execution.skipped_count,execution.failed_count,
                    CASE WHEN execution.status='failed' THEN 'legacy_execution_failed'
                         WHEN execution.status NOT IN ('running','completed','cancelled','partial',
                                                       'recovery_required','archived')
                         THEN 'legacy_status_unknown' END
               FROM execution_runs execution
               JOIN plan_runs plan ON plan.id=execution.plan_id;

             INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT rollback.id,plan.scan_id,rollback.execution_id,
                    CASE rollback.mode WHEN 'dry_run' THEN 'rollback_dry_run'
                                       ELSE 'rollback' END,
                    ROW_NUMBER() OVER(
                        PARTITION BY rollback.execution_id,rollback.mode
                        ORDER BY rollback.started_at,rollback.id
                    ),
                    rollback.execution_id,NULL,'legacy_migration',
                    CASE rollback.status
                      WHEN 'running' THEN 'running' WHEN 'completed' THEN 'completed'
                      WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled'
                      WHEN 'partial' THEN 'partial'
                      WHEN 'recovery_required' THEN 'recovery_required'
                      WHEN 'archived' THEN 'archived' ELSE 'failed' END,
                    rollback.started_at,
                    CASE WHEN rollback.status='running' THEN NULL
                         ELSE COALESCE(rollback.finished_at,rollback.started_at) END,
                    rollback.success_count,rollback.skipped_count,rollback.failed_count,
                    CASE WHEN rollback.status='failed' THEN 'legacy_rollback_failed'
                         WHEN rollback.status NOT IN ('running','completed','cancelled','partial',
                                                      'recovery_required','archived')
                         THEN 'legacy_status_unknown' END
               FROM rollback_runs rollback
               JOIN execution_runs execution ON execution.id=rollback.execution_id
               JOIN plan_runs plan ON plan.id=execution.plan_id;

             INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT recovery.id,roots.root_scan_id,recovery.operation_id,
                    CASE recovery.mode WHEN 'dry_run' THEN 'recovery_dry_run'
                                       ELSE 'recovery' END,
                    ROW_NUMBER() OVER(
                        PARTITION BY recovery.operation_id,recovery.mode
                        ORDER BY recovery.started_at,recovery.id
                    ),
                    journal.attempt_id,NULL,'legacy_migration',
                    CASE recovery.status
                      WHEN 'running' THEN 'running' WHEN 'completed' THEN 'completed'
                      WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled'
                      WHEN 'partial' THEN 'partial'
                      WHEN 'recovery_required' THEN 'recovery_required'
                      WHEN 'archived' THEN 'archived' ELSE 'failed' END,
                    recovery.started_at,
                    CASE WHEN recovery.status='running' THEN NULL
                         ELSE COALESCE(recovery.finished_at,recovery.started_at) END,
                    CASE WHEN recovery.status='completed' THEN 1 ELSE 0 END,0,
                    CASE WHEN recovery.status IN ('failed','partial','recovery_required')
                         THEN 1 ELSE 0 END,
                    COALESCE(recovery.error,
                        CASE WHEN recovery.status='failed' THEN 'legacy_recovery_failed'
                             WHEN recovery.status NOT IN (
                                  'running','completed','cancelled','partial',
                                  'recovery_required','archived')
                             THEN 'legacy_status_unknown' END)
               FROM recovery_runs recovery
               JOIN operation_journal journal ON journal.id=recovery.operation_id
               JOIN (
                    SELECT execution.id attempt_id,plan.scan_id root_scan_id
                      FROM execution_runs execution
                      JOIN plan_runs plan ON plan.id=execution.plan_id
                    UNION ALL
                    SELECT rollback.id,plan.scan_id
                      FROM rollback_runs rollback
                      JOIN execution_runs execution ON execution.id=rollback.execution_id
                      JOIN plan_runs plan ON plan.id=execution.plan_id
               ) roots ON roots.attempt_id=journal.attempt_id;

             INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT verify.id,plan.scan_id,
                    COALESCE(verify.subject_kind,'execution') || ':' ||
                        COALESCE(verify.subject_id,verify.execution_id),
                    'verify',
                    ROW_NUMBER() OVER(
                        PARTITION BY COALESCE(verify.subject_kind,'execution'),
                                     COALESCE(verify.subject_id,verify.execution_id)
                        ORDER BY verify.started_at,verify.id
                    ),
                    COALESCE(verify.subject_id,verify.execution_id),NULL,
                    'legacy_migration',
                    CASE verify.status
                      WHEN 'running' THEN 'running' WHEN 'completed' THEN 'completed'
                      WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled'
                      WHEN 'partial' THEN 'partial'
                      WHEN 'recovery_required' THEN 'recovery_required'
                      WHEN 'archived' THEN 'archived' ELSE 'failed' END,
                    verify.started_at,
                    CASE WHEN verify.status='running' THEN NULL
                         ELSE COALESCE(verify.finished_at,verify.started_at) END,
                    verify.success_count,0,verify.failed_count,
                    CASE WHEN verify.status='failed' THEN 'legacy_verify_failed'
                         WHEN verify.status NOT IN ('running','completed','cancelled','partial',
                                                    'recovery_required','archived')
                         THEN 'legacy_status_unknown' END
               FROM verify_runs verify
               JOIN execution_runs execution ON execution.id=verify.execution_id
               JOIN plan_runs plan ON plan.id=execution.plan_id;

             INSERT OR IGNORE INTO workflow_attempts(
                 id,root_scan_id,logical_run_id,kind,attempt_no,parent_attempt_id,
                 predecessor_attempt_id,actor,status,started_at,finished_at,
                 success_count,skipped_count,failed_count,error_code
             )
             SELECT archive.id,archive.root_scan_id,archive.root_scan_id,'archive',
                    ROW_NUMBER() OVER(
                        PARTITION BY archive.root_scan_id
                        ORDER BY archive.created_at,archive.id
                    ),
                    archive.root_scan_id,NULL,'legacy_migration',
                    CASE archive.status WHEN 'building' THEN 'running'
                                        WHEN 'completed' THEN 'completed'
                                        ELSE 'failed' END,
                    archive.created_at,
                    CASE WHEN archive.status='building' THEN NULL
                         ELSE COALESCE(archive.completed_at,archive.created_at) END,
                    CASE WHEN archive.status='completed' THEN archive.item_count ELSE 0 END,
                    0,CASE WHEN archive.status='failed' THEN 1 ELSE 0 END,
                    CASE WHEN archive.status='failed' THEN 'legacy_archive_failed' END
               FROM archive_manifests archive;

             UPDATE workflow_attempts AS current
                SET predecessor_attempt_id=(
                    SELECT prior.id FROM workflow_attempts prior
                     WHERE prior.logical_run_id=current.logical_run_id
                       AND prior.kind=current.kind
                       AND prior.attempt_no=current.attempt_no-1
                )
              WHERE current.attempt_no>1 AND current.predecessor_attempt_id IS NULL;

             INSERT OR IGNORE INTO operation_journal_events(
                 operation_id,transition_no,from_state,to_state,fencing_token,
                 payload_schema_version,payload_json,created_at
             )
             SELECT id,transition_no,NULL,state,fencing_token,1,
                    '{\"origin\":\"legacy_v10_backfill\",\"history\":\"current_state_only\"}',
                    updated_at
               FROM operation_journal;",
        )
        .map_err(|error| error.to_string())?;
    let orphaned_journals: i64 = transaction
        .query_row(
            "SELECT COUNT(*)
               FROM operation_journal journal
               LEFT JOIN workflow_attempts attempt ON attempt.id=journal.attempt_id
              WHERE attempt.id IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if orphaned_journals != 0 {
        return Err(format!(
            "workflow_attempt_backfill_orphan_journal:{orphaned_journals}"
        ));
    }
    Ok(())
}

fn migrate_v12(transaction: &Transaction<'_>) -> Result<(), String> {
    // A lease owns one fencing token and one or more canonical roots. The v11
    // table made fencing_token unique, which could only represent target-root
    // locking. Rebuild it transactionally while preserving every active row.
    transaction
        .execute_batch(
            "ALTER TABLE mutation_leases RENAME TO mutation_leases_v11;
             CREATE TABLE mutation_leases (
                 scope_key TEXT NOT NULL UNIQUE,
                 scope_path_encoding TEXT NOT NULL,
                 scope_path_blob BLOB NOT NULL,
                 owner_id TEXT NOT NULL,
                 fencing_token INTEGER NOT NULL CHECK(fencing_token > 0),
                 heartbeat_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 PRIMARY KEY(fencing_token,scope_key),
                 CHECK(expires_at > heartbeat_at)
             );
             INSERT INTO mutation_leases(
                 scope_key,scope_path_encoding,scope_path_blob,owner_id,
                 fencing_token,heartbeat_at,expires_at
             )
             SELECT scope_key,scope_path_encoding,scope_path_blob,owner_id,
                    fencing_token,heartbeat_at,expires_at
               FROM mutation_leases_v11;
             DROP TABLE mutation_leases_v11;
             CREATE INDEX idx_mutation_leases_owner_token
                 ON mutation_leases(owner_id,fencing_token,expires_at);

             CREATE TABLE preflight_runs (
                 id TEXT PRIMARY KEY,
                 parent_attempt_id TEXT NOT NULL REFERENCES workflow_attempts(id),
                 kind TEXT NOT NULL CHECK(kind IN ('apply','rollback','recovery')),
                 mode TEXT NOT NULL CHECK(mode IN ('dry_run','mutation')),
                 status TEXT NOT NULL CHECK(status IN (
                     'running','passed','blocked','failed'
                 )),
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 passed_count INTEGER NOT NULL DEFAULT 0 CHECK(passed_count >= 0),
                 blocked_count INTEGER NOT NULL DEFAULT 0 CHECK(blocked_count >= 0),
                 CHECK((status='running' AND finished_at IS NULL)
                       OR (status<>'running' AND finished_at IS NOT NULL)),
                 UNIQUE(parent_attempt_id)
             );
             CREATE INDEX idx_preflight_runs_parent
                 ON preflight_runs(parent_attempt_id,started_at,id);

             CREATE TABLE preflight_logs (
                 preflight_id TEXT NOT NULL REFERENCES preflight_runs(id) ON DELETE CASCADE,
                 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                 item_id TEXT NOT NULL,
                 source_path_encoding TEXT NOT NULL,
                 source_path_blob BLOB NOT NULL,
                 target_path_encoding TEXT,
                 target_path_blob BLOB,
                 outcome TEXT NOT NULL CHECK(outcome IN ('passed','blocked','failed')),
                 code TEXT,
                 expected_size INTEGER CHECK(expected_size IS NULL OR expected_size >= 0),
                 expected_mtime_ns TEXT,
                 expected_content_sha256 TEXT,
                 expected_file_identity TEXT,
                 expected_fingerprint_version INTEGER
                     CHECK(expected_fingerprint_version IS NULL
                           OR expected_fingerprint_version > 0),
                 observed_size INTEGER CHECK(observed_size IS NULL OR observed_size >= 0),
                 observed_mtime_ns TEXT,
                 observed_content_sha256 TEXT,
                 observed_file_identity TEXT,
                 observed_fingerprint_version INTEGER
                     CHECK(observed_fingerprint_version IS NULL
                           OR observed_fingerprint_version > 0),
                 observed_at INTEGER NOT NULL,
                 PRIMARY KEY(preflight_id,ordinal),
                 CHECK((target_path_encoding IS NULL)=(target_path_blob IS NULL))
             );
             CREATE INDEX idx_preflight_logs_outcome
                 ON preflight_logs(preflight_id,outcome,ordinal);
             CREATE TRIGGER preflight_logs_no_update
             BEFORE UPDATE ON preflight_logs
             BEGIN
                 SELECT RAISE(ABORT,'preflight_logs_append_only');
             END;",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v13(transaction: &Transaction<'_>) -> Result<(), String> {
    // Rules and eligibility are separate authorization surfaces. Existing
    // plans remain inspectable but are deliberately marked schema version 0,
    // so they cannot become executable merely because a migration inferred
    // enough fields to display them.
    for (table, column, definition) in [
        (
            "plan_runs",
            "rules_schema_version",
            "INTEGER NOT NULL DEFAULT 0 CHECK(rules_schema_version >= 0)",
        ),
        (
            "plan_items",
            "execution_disposition",
            "TEXT NOT NULL DEFAULT 'blocked' CHECK(execution_disposition IN ('executable','skip','blocked'))",
        ),
        (
            "plan_items",
            "issues_schema_version",
            "INTEGER NOT NULL DEFAULT 0 CHECK(issues_schema_version >= 0)",
        ),
        (
            "plan_items",
            "issues_json",
            "TEXT NOT NULL DEFAULT '[]' CHECK(length(issues_json) > 0)",
        ),
        (
            "metadata_cache_entries",
            "reader_id",
            "TEXT NOT NULL DEFAULT 'legacy-unknown' CHECK(length(reader_id) > 0)",
        ),
        (
            "metadata_cache_entries",
            "reader_config_hash",
            "TEXT NOT NULL DEFAULT 'legacy-unknown' CHECK(length(reader_config_hash) > 0)",
        ),
        (
            "metadata_cache_entries",
            "fingerprint_algorithm",
            "TEXT NOT NULL DEFAULT 'legacy-unknown' CHECK(length(fingerprint_algorithm) > 0)",
        ),
        (
            "metadata_cache_entries",
            "path_normalization_version",
            "INTEGER NOT NULL DEFAULT 0 CHECK(path_normalization_version >= 0)",
        ),
    ] {
        add_column_if_missing(transaction, table, column, definition)?;
    }

    transaction
        .execute_batch(
            "DROP INDEX IF EXISTS idx_metadata_cache_lookup;
             DROP TRIGGER IF EXISTS plan_items_only_while_plan_running;
             DROP TRIGGER IF EXISTS plan_items_completed_no_update;
             DROP TRIGGER IF EXISTS plan_items_completed_no_delete;
             DROP TRIGGER IF EXISTS plan_runs_completed_authorization_immutable;
             DROP TRIGGER IF EXISTS plan_runs_completed_status_immutable;
             UPDATE plan_items
                SET execution_disposition=CASE
                    WHEN risk IN ('invalid_target','path_too_long','conflict') THEN 'blocked'
                    WHEN action='skip' THEN 'skip'
                    ELSE 'executable'
                END;
             CREATE INDEX idx_metadata_cache_lookup
                 ON metadata_cache_entries(
                     path_encoding,path_blob,file_identity,fingerprint_version,
                     content_sha256,fingerprint_algorithm,kind,
                     reader_id,reader_version,schema_version,reader_config_hash,
                     path_normalization_version,id DESC
                 );
             CREATE TRIGGER plan_items_only_while_plan_running
             BEFORE INSERT ON plan_items
             WHEN COALESCE((SELECT status FROM plan_runs WHERE id=NEW.plan_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'plan_not_running');
             END;
             CREATE TRIGGER plan_items_completed_no_update
             BEFORE UPDATE ON plan_items
             WHEN EXISTS(SELECT 1 FROM plan_runs
                          WHERE id=OLD.plan_id AND status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_items_immutable');
             END;
             CREATE TRIGGER plan_items_completed_no_delete
             BEFORE DELETE ON plan_items
             WHEN EXISTS(SELECT 1 FROM plan_runs
                          WHERE id=OLD.plan_id AND status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_items_immutable');
             END;
             CREATE TRIGGER plan_runs_completed_authorization_immutable
             BEFORE UPDATE OF scan_id,parent_plan_id,target_root,target_root_encoding,
                              target_root_blob,scan_snapshot_hash,rules_json,
                              rules_schema_version,snapshot_version,path_policy_version,
                              snapshot_hash
             ON plan_runs
             WHEN OLD.status='completed'
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_authorization_immutable');
             END;
             CREATE TRIGGER plan_runs_completed_status_immutable
             BEFORE UPDATE OF status ON plan_runs
             WHEN OLD.status='completed' AND NEW.status<>OLD.status
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_status_immutable');
             END;",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v14(transaction: &Transaction<'_>) -> Result<(), String> {
    add_column_if_missing(
        transaction,
        "archive_manifests",
        "archive_path_encoding",
        "TEXT",
    )?;
    add_column_if_missing(
        transaction,
        "archive_manifests",
        "archive_path_blob",
        "BLOB",
    )?;
    transaction
        .execute_batch(
            "UPDATE archive_manifests
                SET archive_path_encoding=COALESCE(
                        archive_path_encoding,'utf8_legacy_v1'
                    ),
                    archive_path_blob=COALESCE(
                        archive_path_blob,CAST(archive_path AS BLOB)
                    );",
        )
        .map_err(|error| error.to_string())
}

fn migrate_v15(transaction: &Transaction<'_>) -> Result<(), String> {
    add_column_if_missing(
        transaction,
        "plan_conflict_candidates",
        "target_path_encoding",
        "TEXT",
    )?;
    add_column_if_missing(
        transaction,
        "plan_conflict_candidates",
        "target_path_blob",
        "BLOB",
    )?;
    transaction
        .execute_batch(
            "DROP TRIGGER IF EXISTS plan_conflict_groups_only_while_running;
             DROP TRIGGER IF EXISTS plan_conflict_groups_completed_immutable;
             DROP TRIGGER IF EXISTS plan_conflict_groups_completed_no_delete;
             DROP TRIGGER IF EXISTS plan_conflict_members_only_while_running;
             DROP TRIGGER IF EXISTS plan_conflict_members_completed_immutable;
             DROP TRIGGER IF EXISTS plan_conflict_members_completed_no_delete;
             DROP TRIGGER IF EXISTS plan_conflict_candidates_only_while_running;
             DROP TRIGGER IF EXISTS plan_conflict_candidates_lossless_required_insert;
             DROP TRIGGER IF EXISTS plan_conflict_candidates_lossless_required_update;
             DROP TRIGGER IF EXISTS plan_conflict_candidates_completed_immutable;
             DROP TRIGGER IF EXISTS plan_conflict_candidates_completed_no_delete;
             DROP TRIGGER IF EXISTS plan_conflict_candidate_members_only_while_running;
             DROP TRIGGER IF EXISTS plan_conflict_candidate_members_completed_immutable;
             DROP TRIGGER IF EXISTS plan_conflict_candidate_members_completed_no_delete;

             UPDATE plan_conflict_candidates
                SET target_path_encoding=COALESCE(target_path_encoding,'utf8_legacy_v1'),
                    target_path_blob=COALESCE(target_path_blob,CAST(target_path AS BLOB));

             CREATE TRIGGER plan_conflict_groups_only_while_running
             BEFORE INSERT ON plan_conflict_groups
             WHEN COALESCE((SELECT status FROM plan_runs WHERE id=NEW.plan_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'plan_not_running');
             END;
             CREATE TRIGGER plan_conflict_groups_completed_immutable
             BEFORE UPDATE ON plan_conflict_groups
             WHEN EXISTS(SELECT 1 FROM plan_runs
                          WHERE id=OLD.plan_id AND status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_groups_completed_no_delete
             BEFORE DELETE ON plan_conflict_groups
             WHEN EXISTS(SELECT 1 FROM plan_runs
                          WHERE id=OLD.plan_id AND status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;

             CREATE TRIGGER plan_conflict_members_only_while_running
             BEFORE INSERT ON plan_conflict_members
             WHEN COALESCE((SELECT plan.status FROM plan_conflict_groups group_row
                            JOIN plan_runs plan ON plan.id=group_row.plan_id
                            WHERE group_row.id=NEW.conflict_group_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'plan_not_running');
             END;
             CREATE TRIGGER plan_conflict_members_completed_immutable
             BEFORE UPDATE ON plan_conflict_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                         JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id=OLD.conflict_group_id
                           AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_members_completed_no_delete
             BEFORE DELETE ON plan_conflict_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                         JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id=OLD.conflict_group_id
                           AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;

             CREATE TRIGGER plan_conflict_candidates_only_while_running
             BEFORE INSERT ON plan_conflict_candidates
             WHEN COALESCE((SELECT plan.status FROM plan_conflict_groups group_row
                            JOIN plan_runs plan ON plan.id=group_row.plan_id
                            WHERE group_row.id=NEW.conflict_group_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'plan_not_running');
             END;
             CREATE TRIGGER plan_conflict_candidates_lossless_required_insert
             BEFORE INSERT ON plan_conflict_candidates
             WHEN NEW.target_path_encoding IS NULL OR NEW.target_path_blob IS NULL
             BEGIN
                 SELECT RAISE(ABORT,'plan_conflict_candidate_path_encoding_required');
             END;
             CREATE TRIGGER plan_conflict_candidates_lossless_required_update
             BEFORE UPDATE OF target_path_encoding,target_path_blob
             ON plan_conflict_candidates
             WHEN NEW.target_path_encoding IS NULL OR NEW.target_path_blob IS NULL
             BEGIN
                 SELECT RAISE(ABORT,'plan_conflict_candidate_path_encoding_required');
             END;
             CREATE TRIGGER plan_conflict_candidates_completed_immutable
             BEFORE UPDATE ON plan_conflict_candidates
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                         JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id=OLD.conflict_group_id
                           AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_candidates_completed_no_delete
             BEFORE DELETE ON plan_conflict_candidates
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                         JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id=OLD.conflict_group_id
                           AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;

             CREATE TRIGGER plan_conflict_candidate_members_only_while_running
             BEFORE INSERT ON plan_conflict_candidate_members
             WHEN COALESCE((SELECT plan.status FROM plan_conflict_groups group_row
                            JOIN plan_runs plan ON plan.id=group_row.plan_id
                            WHERE group_row.id=NEW.conflict_group_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'plan_not_running');
             END;
             CREATE TRIGGER plan_conflict_candidate_members_completed_immutable
             BEFORE UPDATE ON plan_conflict_candidate_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                         JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id=OLD.conflict_group_id
                           AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_candidate_members_completed_no_delete
             BEFORE DELETE ON plan_conflict_candidate_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                         JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id=OLD.conflict_group_id
                           AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;",
        )
        .map_err(|error| error.to_string())?;
    let incomplete_candidates: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM plan_conflict_candidates
              WHERE target_path_encoding IS NULL OR target_path_blob IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if incomplete_candidates != 0 {
        return Err(format!(
            "plan_conflict_candidate_path_backfill_incomplete:{incomplete_candidates}"
        ));
    }
    Ok(())
}

fn migrate_v16(transaction: &Transaction<'_>) -> Result<(), String> {
    transaction
        .execute_batch(
            "DROP TABLE IF EXISTS scan_items_v16_new;
             DROP TRIGGER IF EXISTS scan_items_no_update;
             DROP TRIGGER IF EXISTS scan_items_only_while_running;
             DROP TRIGGER IF EXISTS plan_items_conflict_group_same_plan_insert;
             DROP TRIGGER IF EXISTS plan_items_conflict_group_same_plan_update;
             DROP TRIGGER IF EXISTS plan_conflict_groups_same_plan_insert;
             DROP TRIGGER IF EXISTS plan_conflict_groups_same_plan_update;
             DROP TRIGGER IF EXISTS plan_conflict_members_same_plan_insert;
             DROP TRIGGER IF EXISTS plan_conflict_members_same_plan_update;
             DROP TRIGGER IF EXISTS plan_conflict_candidates_same_plan_update;
             DROP TRIGGER IF EXISTS plan_conflict_candidate_members_same_plan_insert;
             DROP TRIGGER IF EXISTS plan_conflict_candidate_members_same_plan_update;
             DROP TRIGGER IF EXISTS plan_runs_completed_graph_scope;
             CREATE TABLE scan_items_v16_new (
                 scan_id TEXT NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
                 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                 path TEXT NOT NULL,
                 path_encoding TEXT NOT NULL CHECK(length(path_encoding) > 0),
                 path_blob BLOB NOT NULL,
                 path_key_version INTEGER NOT NULL CHECK(path_key_version > 0),
                 path_key TEXT NOT NULL CHECK(length(path_key) > 0),
                 size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                 mtime_ns TEXT NOT NULL CHECK(length(mtime_ns) > 0),
                 content_sha256 TEXT,
                 file_identity TEXT,
                 fingerprint_version INTEGER NOT NULL CHECK(fingerprint_version > 0),
                 metadata_json TEXT,
                 metadata_status TEXT NOT NULL
                     CHECK(metadata_status IN ('ok','error','not_applicable')),
                 kind TEXT NOT NULL CHECK(kind IN ('music','image')),
                 CHECK(
                     (metadata_status='ok' AND metadata_json IS NOT NULL) OR
                     (metadata_status!='ok' AND metadata_json IS NULL)
                 ),
                 PRIMARY KEY(scan_id,ordinal)
             );",
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "WITH numbered AS (
                 SELECT scan_id,path,path_encoding,path_blob,size_bytes,mtime_ns,
                        content_sha256,file_identity,fingerprint_version,
                        metadata_json,metadata_status,kind,
                        ROW_NUMBER() OVER (
                            PARTITION BY scan_id
                            ORDER BY path_encoding,path_blob,path
                        ) - 1 AS ordinal
                   FROM scan_items
             )
             INSERT INTO scan_items_v16_new(
                 scan_id,ordinal,path,path_encoding,path_blob,path_key_version,path_key,
                 size_bytes,mtime_ns,content_sha256,file_identity,fingerprint_version,
                 metadata_json,metadata_status,kind
             )
             SELECT scan_id,ordinal,path,path_encoding,path_blob,?1,
                    'v16-pending-' || printf('%016x',ordinal),
                    size_bytes,mtime_ns,content_sha256,file_identity,fingerprint_version,
                    metadata_json,metadata_status,kind
               FROM numbered",
            params![i64::from(WINDOWS_PATH_KEY_VERSION)],
        )
        .map_err(|error| error.to_string())?;

    let encoded_paths = {
        let mut statement = transaction
            .prepare(
                "SELECT scan_id,ordinal,path_encoding,path_blob
                   FROM scan_items_v16_new ORDER BY scan_id,ordinal",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    for (scan_id, ordinal, encoding, raw) in encoded_paths {
        let path = decode_path(&encoding, &raw)?;
        transaction
            .execute(
                "UPDATE scan_items_v16_new SET path_key=?3
                  WHERE scan_id=?1 AND ordinal=?2",
                params![scan_id, ordinal, windows_path_key(&path)],
            )
            .map_err(|error| error.to_string())?;
    }
    let duplicate_lossless_keys: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM (
                 SELECT 1 FROM scan_items_v16_new
                  GROUP BY scan_id,path_key_version,path_key HAVING COUNT(*)>1
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if duplicate_lossless_keys != 0 {
        return Err(format!(
            "scan_item_path_key_backfill_conflict:{duplicate_lossless_keys}"
        ));
    }

    let invalid_plan_graph_rows: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM (
                 SELECT item.id
                   FROM plan_items item
                   LEFT JOIN plan_conflict_groups group_row
                     ON group_row.id=item.conflict_group_id
                  WHERE item.conflict_group_id IS NOT NULL
                    AND (group_row.id IS NULL OR group_row.plan_id<>item.plan_id)
                 UNION ALL
                 SELECT member.conflict_group_id || ':' || member.plan_item_id
                   FROM plan_conflict_members member
                   LEFT JOIN plan_conflict_groups group_row
                     ON group_row.id=member.conflict_group_id
                   LEFT JOIN plan_items item ON item.id=member.plan_item_id
                  WHERE group_row.id IS NULL OR item.id IS NULL
                     OR group_row.plan_id<>item.plan_id
                 UNION ALL
                 SELECT member.conflict_group_id || ':' || member.candidate_ordinal
                        || ':' || member.plan_item_id
                   FROM plan_conflict_candidate_members member
                   LEFT JOIN plan_conflict_groups group_row
                     ON group_row.id=member.conflict_group_id
                   LEFT JOIN plan_items item ON item.id=member.plan_item_id
                  WHERE group_row.id IS NULL OR item.id IS NULL
                     OR group_row.plan_id<>item.plan_id
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if invalid_plan_graph_rows != 0 {
        return Err(format!(
            "plan_graph_scope_backfill_invalid:{invalid_plan_graph_rows}"
        ));
    }

    transaction
        .execute_batch(
            "DROP TABLE scan_items;
             ALTER TABLE scan_items_v16_new RENAME TO scan_items;
             CREATE INDEX idx_scan_items_scan ON scan_items(scan_id,ordinal);
             CREATE UNIQUE INDEX idx_scan_items_path_blob
                 ON scan_items(scan_id,path_encoding,path_blob);
             CREATE UNIQUE INDEX idx_scan_items_path_key
                 ON scan_items(scan_id,path_key_version,path_key);
             CREATE TRIGGER scan_items_no_update
             BEFORE UPDATE ON scan_items
             BEGIN
                 SELECT RAISE(ABORT,'scan_items_immutable');
             END;
             CREATE TRIGGER scan_items_only_while_running
             BEFORE INSERT ON scan_items
             WHEN COALESCE((SELECT status FROM scan_runs WHERE id=NEW.scan_id),'')!='running'
             BEGIN
                 SELECT RAISE(ABORT,'scan_not_running');
             END;

             DROP TRIGGER IF EXISTS plan_items_completed_no_update;
             CREATE TRIGGER plan_items_completed_no_update
             BEFORE UPDATE ON plan_items
             WHEN EXISTS(SELECT 1 FROM plan_runs
                          WHERE id IN (OLD.plan_id,NEW.plan_id) AND status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_items_immutable');
             END;
             CREATE TRIGGER plan_items_conflict_group_same_plan_insert
             BEFORE INSERT ON plan_items
             WHEN NEW.conflict_group_id IS NOT NULL
              AND EXISTS(SELECT 1 FROM plan_conflict_groups
                          WHERE id=NEW.conflict_group_id)
              AND NOT EXISTS(SELECT 1 FROM plan_conflict_groups
                              WHERE id=NEW.conflict_group_id AND plan_id=NEW.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;
             CREATE TRIGGER plan_items_conflict_group_same_plan_update
             BEFORE UPDATE OF plan_id,conflict_group_id ON plan_items
             WHEN (NEW.conflict_group_id IS NOT NULL
                   AND EXISTS(SELECT 1 FROM plan_conflict_groups
                               WHERE id=NEW.conflict_group_id)
                   AND NOT EXISTS(SELECT 1 FROM plan_conflict_groups
                                   WHERE id=NEW.conflict_group_id
                                     AND plan_id=NEW.plan_id))
               OR EXISTS(SELECT 1 FROM plan_conflict_members member
                          JOIN plan_conflict_groups group_row
                            ON group_row.id=member.conflict_group_id
                         WHERE member.plan_item_id=OLD.id
                           AND group_row.plan_id<>NEW.plan_id)
               OR EXISTS(SELECT 1 FROM plan_conflict_candidate_members member
                          JOIN plan_conflict_groups group_row
                            ON group_row.id=member.conflict_group_id
                         WHERE member.plan_item_id=OLD.id
                           AND group_row.plan_id<>NEW.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;

             DROP TRIGGER IF EXISTS plan_conflict_groups_completed_immutable;
             CREATE TRIGGER plan_conflict_groups_completed_immutable
             BEFORE UPDATE ON plan_conflict_groups
             WHEN EXISTS(SELECT 1 FROM plan_runs
                          WHERE id IN (OLD.plan_id,NEW.plan_id) AND status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_groups_same_plan_insert
             BEFORE INSERT ON plan_conflict_groups
             WHEN EXISTS(SELECT 1 FROM plan_items item
                          WHERE item.conflict_group_id=NEW.id
                            AND item.plan_id<>NEW.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;
             CREATE TRIGGER plan_conflict_groups_same_plan_update
             BEFORE UPDATE OF id,plan_id ON plan_conflict_groups
             WHEN EXISTS(SELECT 1 FROM plan_items item
                          WHERE item.conflict_group_id=OLD.id
                            AND item.plan_id<>NEW.plan_id)
               OR EXISTS(SELECT 1 FROM plan_conflict_members member
                          JOIN plan_items item ON item.id=member.plan_item_id
                         WHERE member.conflict_group_id=OLD.id
                           AND item.plan_id<>NEW.plan_id)
               OR EXISTS(SELECT 1 FROM plan_conflict_candidate_members member
                          JOIN plan_items item ON item.id=member.plan_item_id
                         WHERE member.conflict_group_id=OLD.id
                           AND item.plan_id<>NEW.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;

             DROP TRIGGER IF EXISTS plan_conflict_members_completed_immutable;
             CREATE TRIGGER plan_conflict_members_completed_immutable
             BEFORE UPDATE ON plan_conflict_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                          JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id IN (
                                   OLD.conflict_group_id,NEW.conflict_group_id
                               ) AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_members_same_plan_insert
             BEFORE INSERT ON plan_conflict_members
             WHEN COALESCE((SELECT group_row.plan_id=item.plan_id
                              FROM plan_conflict_groups group_row
                              JOIN plan_items item ON item.id=NEW.plan_item_id
                             WHERE group_row.id=NEW.conflict_group_id),0)=0
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;
             CREATE TRIGGER plan_conflict_members_same_plan_update
             BEFORE UPDATE OF conflict_group_id,plan_item_id ON plan_conflict_members
             WHEN COALESCE((SELECT group_row.plan_id=item.plan_id
                              FROM plan_conflict_groups group_row
                              JOIN plan_items item ON item.id=NEW.plan_item_id
                             WHERE group_row.id=NEW.conflict_group_id),0)=0
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;

             DROP TRIGGER IF EXISTS plan_conflict_candidates_completed_immutable;
             CREATE TRIGGER plan_conflict_candidates_completed_immutable
             BEFORE UPDATE ON plan_conflict_candidates
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                          JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id IN (
                                   OLD.conflict_group_id,NEW.conflict_group_id
                               ) AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_candidates_same_plan_update
             BEFORE UPDATE OF conflict_group_id,ordinal ON plan_conflict_candidates
             WHEN EXISTS(SELECT 1 FROM plan_conflict_candidate_members member
                          JOIN plan_items item ON item.id=member.plan_item_id
                          JOIN plan_conflict_groups group_row
                            ON group_row.id=NEW.conflict_group_id
                         WHERE member.conflict_group_id=OLD.conflict_group_id
                           AND member.candidate_ordinal=OLD.ordinal
                           AND item.plan_id<>group_row.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;

             DROP TRIGGER IF EXISTS plan_conflict_candidate_members_completed_immutable;
             CREATE TRIGGER plan_conflict_candidate_members_completed_immutable
             BEFORE UPDATE ON plan_conflict_candidate_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                          JOIN plan_runs plan ON plan.id=group_row.plan_id
                         WHERE group_row.id IN (
                                   OLD.conflict_group_id,NEW.conflict_group_id
                               ) AND plan.status='completed')
             BEGIN
                 SELECT RAISE(ABORT,'completed_plan_conflicts_immutable');
             END;
             CREATE TRIGGER plan_conflict_candidate_members_same_plan_insert
             BEFORE INSERT ON plan_conflict_candidate_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                          JOIN plan_items item ON item.id=NEW.plan_item_id
                          JOIN plan_conflict_candidates candidate
                            ON candidate.conflict_group_id=NEW.conflict_group_id
                           AND candidate.ordinal=NEW.candidate_ordinal
                         WHERE group_row.id=NEW.conflict_group_id
                           AND group_row.plan_id<>item.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;
             CREATE TRIGGER plan_conflict_candidate_members_same_plan_update
             BEFORE UPDATE OF conflict_group_id,candidate_ordinal,plan_item_id
             ON plan_conflict_candidate_members
             WHEN EXISTS(SELECT 1 FROM plan_conflict_groups group_row
                          JOIN plan_items item ON item.id=NEW.plan_item_id
                          JOIN plan_conflict_candidates candidate
                            ON candidate.conflict_group_id=NEW.conflict_group_id
                           AND candidate.ordinal=NEW.candidate_ordinal
                         WHERE group_row.id=NEW.conflict_group_id
                           AND group_row.plan_id<>item.plan_id)
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;

             CREATE TRIGGER plan_runs_completed_graph_scope
             BEFORE UPDATE OF status ON plan_runs
             WHEN OLD.status<>'completed' AND NEW.status='completed' AND (
                 EXISTS(SELECT 1 FROM plan_items item
                        LEFT JOIN plan_conflict_groups group_row
                          ON group_row.id=item.conflict_group_id
                       WHERE item.plan_id=NEW.id AND item.conflict_group_id IS NOT NULL
                         AND (group_row.id IS NULL OR group_row.plan_id<>NEW.id))
                 OR EXISTS(SELECT 1 FROM plan_conflict_members member
                           JOIN plan_conflict_groups group_row
                             ON group_row.id=member.conflict_group_id
                           JOIN plan_items item ON item.id=member.plan_item_id
                          WHERE group_row.plan_id=NEW.id AND item.plan_id<>NEW.id)
                 OR EXISTS(SELECT 1 FROM plan_conflict_candidate_members member
                           JOIN plan_conflict_groups group_row
                             ON group_row.id=member.conflict_group_id
                           LEFT JOIN plan_conflict_candidates candidate
                             ON candidate.conflict_group_id=member.conflict_group_id
                            AND candidate.ordinal=member.candidate_ordinal
                           LEFT JOIN plan_items item ON item.id=member.plan_item_id
                          WHERE group_row.plan_id=NEW.id
                            AND (candidate.conflict_group_id IS NULL
                                 OR item.id IS NULL OR item.plan_id<>NEW.id))
             )
             BEGIN
                 SELECT RAISE(ABORT,'plan_graph_scope_mismatch');
             END;",
        )
        .map_err(|error| error.to_string())
}

fn validate_latest_schema(connection: &Connection) -> Result<(), String> {
    for (table, columns) in [
        (
            "scan_runs",
            &["snapshot_hash", "snapshot_version", "source_root_blob"][..],
        ),
        (
            "scan_items",
            &[
                "ordinal",
                "path",
                "size_bytes",
                "mtime_ns",
                "metadata_json",
                "metadata_status",
                "kind",
                "path_encoding",
                "path_blob",
                "path_key_version",
                "path_key",
                "content_sha256",
                "file_identity",
                "fingerprint_version",
            ][..],
        ),
        (
            "plan_items",
            &[
                "source_size_bytes",
                "source_mtime_ns",
                "source_file_identity",
                "source_identity_version",
                "source_content_hash",
                "source_hash_algorithm",
                "source_hash_version",
                "source_path_encoding",
                "source_path_blob",
                "target_path_encoding",
                "target_path_blob",
                "execution_disposition",
                "issues_schema_version",
                "issues_json",
            ][..],
        ),
        ("plan_runs", &["rules_schema_version"][..]),
        (
            "plan_conflict_candidates",
            &["target_path_encoding", "target_path_blob"][..],
        ),
        (
            "metadata_cache_entries",
            &[
                "reader_id",
                "reader_config_hash",
                "fingerprint_algorithm",
                "path_normalization_version",
            ][..],
        ),
        (
            "mutation_leases",
            &["scope_path_encoding", "scope_path_blob", "fencing_token"][..],
        ),
        (
            "preflight_runs",
            &[
                "parent_attempt_id",
                "kind",
                "mode",
                "status",
                "passed_count",
                "blocked_count",
            ][..],
        ),
        (
            "preflight_logs",
            &[
                "preflight_id",
                "ordinal",
                "item_id",
                "source_path_encoding",
                "source_path_blob",
                "target_path_encoding",
                "target_path_blob",
                "outcome",
                "code",
                "expected_fingerprint_version",
                "observed_fingerprint_version",
                "observed_at",
            ][..],
        ),
        (
            "operation_logs",
            &[
                "source_path_encoding",
                "source_path_blob",
                "target_path_encoding",
                "target_path_blob",
            ][..],
        ),
        (
            "archive_manifests",
            &[
                "digest_algorithm",
                "byte_count",
                "verified_at",
                "archive_path_encoding",
                "archive_path_blob",
            ][..],
        ),
        (
            "plan_builds",
            &["phase", "cursor_ordinal", "peak_batch_items"][..],
        ),
        (
            "plan_item_staging",
            &[
                "target_path_encoding",
                "target_path_blob",
                "original_target_key",
                "parent_plan_item_id",
                "change_origin",
                "change_reason",
            ][..],
        ),
        ("plan_items", &["source_kind", "source_metadata_json"][..]),
        ("recovery_runs", &["action"][..]),
        (
            "operation_journal",
            &["strategy", "staged_file_identity", "transition_no"][..],
        ),
        (
            "workflow_attempts",
            &[
                "root_scan_id",
                "logical_run_id",
                "kind",
                "attempt_no",
                "parent_attempt_id",
                "predecessor_attempt_id",
                "actor",
                "status",
                "success_count",
                "skipped_count",
                "failed_count",
                "error_code",
            ][..],
        ),
        (
            "operation_journal_events",
            &[
                "operation_id",
                "transition_no",
                "from_state",
                "to_state",
                "fencing_token",
                "payload_schema_version",
                "payload_json",
                "created_at",
            ][..],
        ),
    ] {
        for column in columns {
            if !has_column(connection, table, column)? {
                return Err(format!("schema_column_missing:{table}.{column}"));
            }
        }
    }
    for index in [
        "idx_scan_items_scan",
        "idx_metadata_cache_lookup",
        "idx_plan_items_source_expectation",
        "idx_scan_items_path_blob",
        "idx_scan_items_path_key",
        "idx_operation_journal_recovery",
        "idx_verify_logs_attempt",
        "idx_rollback_logs_attempt",
        "idx_operation_logs_source_blob",
        "idx_archive_manifests_root",
        "idx_plan_builds_plan",
        "idx_plan_staging_original_target",
        "idx_plan_anchor_source",
        "idx_recovery_runs_action",
        "idx_workflow_attempts_root",
        "idx_workflow_attempts_parent",
        "idx_operation_journal_events_created",
        "idx_mutation_leases_owner_token",
        "idx_preflight_runs_parent",
        "idx_preflight_logs_outcome",
    ] {
        if !has_index(connection, index)? {
            return Err(format!("schema_index_missing:{index}"));
        }
    }
    let foreign_key_violation: Option<String> = connection
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(table) = foreign_key_violation {
        return Err(format!("schema_foreign_key_violation:{table}"));
    }
    Ok(())
}

pub fn upgrade(connection: &mut Connection, applied_at: i64) -> Result<(), String> {
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if user_version > LATEST_SCHEMA_VERSION {
        return Err(format!(
            "database_schema_too_new:{user_version}>{LATEST_SCHEMA_VERSION}"
        ));
    }
    let newest: Option<i64> = connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;
    if newest.is_some_and(|version| version > LATEST_SCHEMA_VERSION) {
        return Err(format!(
            "database_schema_too_new:{}>{LATEST_SCHEMA_VERSION}",
            newest.unwrap_or_default()
        ));
    }

    for version in 1..=LATEST_SCHEMA_VERSION {
        let applied = connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE version=?1",
                params![version],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .is_some();
        if applied {
            continue;
        }

        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        match version {
            1 => migrate_v1(&transaction)?,
            2 => migrate_v2(&transaction)?,
            3 => migrate_v3(&transaction)?,
            4 => migrate_v4(&transaction, applied_at)?,
            5 => migrate_v5(&transaction)?,
            6 => migrate_v6(&transaction)?,
            7 => migrate_v7(&transaction)?,
            8 => migrate_v8(&transaction)?,
            9 => migrate_v9(&transaction)?,
            10 => migrate_v10(&transaction)?,
            11 => migrate_v11(&transaction)?,
            12 => migrate_v12(&transaction)?,
            13 => migrate_v13(&transaction)?,
            14 => migrate_v14(&transaction)?,
            15 => migrate_v15(&transaction)?,
            16 => migrate_v16(&transaction)?,
            _ => unreachable!(),
        }
        transaction
            .execute(
                "INSERT INTO schema_migrations(version,applied_at) VALUES(?1,?2)",
                params![version, applied_at],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
    }
    validate_latest_schema(connection)?;
    connection
        .pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)
        .map_err(|error| error.to_string())
}
