use music_folder_core::{
    assess_windows_path,
    ports::{
        ApplyStore, ManualTargetChange, PlanRevisionStore, PlanStore, RollbackStore, ScanStore,
        VerifyStore,
    },
    ApplyItem, FileFingerprint, FileKind, OperationLog, PlanAction, PlanItem, Risk, ScannedFile,
    TrackMetadata, VerifyItem,
};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug, serde::Serialize)]
pub struct HistoryRow {
    pub id: String,
    pub kind: String,
    pub mode: Option<String>,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub parent_id: Option<String>,
    pub root_scan_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}
#[derive(Debug, serde::Serialize)]
pub struct RunDetailRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub parent_id: Option<String>,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}
#[derive(Debug, serde::Serialize)]
pub struct PlanItemRow {
    pub id: String,
    pub conflict_group_id: Option<String>,
    pub conflict_member_count: u64,
    pub ordinal: u64,
    pub source_path: String,
    pub target_path: Option<String>,
    pub action: String,
    pub risk: String,
    pub reason: Option<String>,
}
#[derive(Debug, serde::Serialize)]
pub struct PlanConflictMemberRow {
    pub item_id: String,
    pub ordinal: u64,
    pub source_path: String,
}
#[derive(Debug, serde::Serialize)]
pub struct PlanConflictDetail {
    pub id: String,
    pub kind: String,
    pub target_path: String,
    pub existing_target_path: Option<String>,
    pub members: Vec<PlanConflictMemberRow>,
}
#[derive(Debug, Default, serde::Serialize)]
pub struct PlanItemCounts {
    pub moves: u64,
    pub skips: u64,
    pub needs_attention: u64,
    pub conflicts: u64,
    pub invalid_target: u64,
    pub metadata_missing: u64,
    pub path_too_long: u64,
}
#[derive(Debug, serde::Serialize)]
pub struct PlanItemPage {
    pub items: Vec<PlanItemRow>,
    pub total: u64,
    pub filtered_total: u64,
    pub next_cursor: Option<u64>,
    pub counts: PlanItemCounts,
}
#[derive(serde::Serialize)]
pub struct OperationLogRow {
    pub id: String,
    pub execution_id: String,
    pub sequence_no: u64,
    pub source_path: String,
    pub target_path: Option<String>,
    pub action: String,
    pub result: String,
    pub error: Option<String>,
    pub created_at: i64,
}
#[derive(serde::Serialize)]
pub struct MetricRow {
    pub phase: String,
    pub elapsed_ms: u64,
    pub item_count: u64,
}
#[derive(serde::Serialize)]
pub struct HistoryCleanupPreview {
    pub plans: u64,
    pub executions: u64,
    pub logs: u64,
    pub blocked: bool,
}

pub struct SqliteScanStore {
    connection: Mutex<Connection>,
}
impl SqliteScanStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS scan_runs (id TEXT PRIMARY KEY, source_root TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, warning_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS library_files (path TEXT PRIMARY KEY, size_bytes INTEGER NOT NULL, mtime_ns TEXT NOT NULL, metadata_json TEXT, metadata_status TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'music', last_seen_scan_id TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS scan_items (scan_id TEXT NOT NULL REFERENCES scan_runs(id), path TEXT NOT NULL, PRIMARY KEY(scan_id,path));
          CREATE TABLE IF NOT EXISTS scan_warnings (id TEXT PRIMARY KEY, scan_id TEXT NOT NULL REFERENCES scan_runs(id), warning TEXT NOT NULL, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS plan_runs (id TEXT PRIMARY KEY, scan_id TEXT NOT NULL REFERENCES scan_runs(id), parent_plan_id TEXT, target_root TEXT NOT NULL, rules_json TEXT, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, conflict_count INTEGER NOT NULL DEFAULT 0, risk_count INTEGER NOT NULL DEFAULT 0, snapshot_hash TEXT);
          CREATE TABLE IF NOT EXISTS plan_items (id TEXT PRIMARY KEY, plan_id TEXT NOT NULL REFERENCES plan_runs(id), ordinal INTEGER NOT NULL, source_path TEXT NOT NULL, target_path TEXT, target_origin TEXT NOT NULL DEFAULT 'rule', action TEXT NOT NULL, risk TEXT NOT NULL, reason TEXT);
          CREATE TABLE IF NOT EXISTS plan_conflict_groups (id TEXT PRIMARY KEY, plan_id TEXT NOT NULL REFERENCES plan_runs(id) ON DELETE CASCADE, kind TEXT NOT NULL, normalized_target_path TEXT NOT NULL, target_path TEXT NOT NULL, existing_target_path TEXT);
          CREATE TABLE IF NOT EXISTS plan_conflict_members (conflict_group_id TEXT NOT NULL REFERENCES plan_conflict_groups(id) ON DELETE CASCADE, plan_item_id TEXT NOT NULL REFERENCES plan_items(id) ON DELETE CASCADE, PRIMARY KEY(conflict_group_id,plan_item_id));
          CREATE TABLE IF NOT EXISTS execution_runs (id TEXT PRIMARY KEY, plan_id TEXT NOT NULL REFERENCES plan_runs(id), mode TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, success_count INTEGER NOT NULL DEFAULT 0, skipped_count INTEGER NOT NULL DEFAULT 0, failed_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS operation_logs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), plan_item_id TEXT NOT NULL REFERENCES plan_items(id), sequence_no INTEGER NOT NULL, source_path TEXT NOT NULL, target_path TEXT, action TEXT NOT NULL, result TEXT NOT NULL, error TEXT, source_deleted INTEGER NOT NULL, expected_size INTEGER, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS verify_logs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), operation_id TEXT NOT NULL REFERENCES operation_logs(id), result TEXT NOT NULL, error TEXT, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS rollback_logs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), operation_id TEXT NOT NULL REFERENCES operation_logs(id), result TEXT NOT NULL, error TEXT, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS verify_runs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, success_count INTEGER NOT NULL DEFAULT 0, failed_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS rollback_runs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), mode TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, success_count INTEGER NOT NULL DEFAULT 0, skipped_count INTEGER NOT NULL DEFAULT 0, failed_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS run_metrics (run_id TEXT NOT NULL, phase TEXT NOT NULL, elapsed_ms INTEGER NOT NULL, item_count INTEGER NOT NULL DEFAULT 0);
          CREATE INDEX IF NOT EXISTS idx_scan_items_scan ON scan_items(scan_id);
          CREATE INDEX IF NOT EXISTS idx_plan_items_plan ON plan_items(plan_id, ordinal);
          CREATE INDEX IF NOT EXISTS idx_plan_conflict_groups_plan_target ON plan_conflict_groups(plan_id, normalized_target_path);
          CREATE INDEX IF NOT EXISTS idx_operation_logs_execution ON operation_logs(execution_id, sequence_no);") .map_err(|e| e.to_string())?;
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(1,?1)",
                params![now()],
            )
            .map_err(|e| e.to_string())?;
        // Upgrade databases made by versions before snapshot hashes existed.
        let has_snapshot = connection
            .prepare("PRAGMA table_info(plan_runs)")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| e.to_string())?
            .iter()
            .any(|name| name == "snapshot_hash");
        if !has_snapshot {
            connection
                .execute_batch("ALTER TABLE plan_runs ADD COLUMN snapshot_hash TEXT;")
                .map_err(|e| e.to_string())?;
        }
        let has_expected_size = connection
            .prepare("PRAGMA table_info(operation_logs)")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|error| error.to_string())?
            .iter()
            .any(|name| name == "expected_size");
        if !has_expected_size {
            connection
                .execute_batch("ALTER TABLE operation_logs ADD COLUMN expected_size INTEGER;")
                .map_err(|error| error.to_string())?;
        }
        let has_kind = connection
            .prepare("PRAGMA table_info(library_files)")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| e.to_string())?
            .iter()
            .any(|name| name == "kind");
        if !has_kind {
            connection
                .execute_batch(
                    "ALTER TABLE library_files ADD COLUMN kind TEXT NOT NULL DEFAULT 'music';",
                )
                .map_err(|e| e.to_string())?;
        }
        for (table, column, definition) in [
            ("plan_runs", "parent_plan_id", "TEXT"),
            ("plan_runs", "rules_json", "TEXT"),
            (
                "plan_items",
                "target_origin",
                "TEXT NOT NULL DEFAULT 'rule'",
            ),
            ("plan_items", "conflict_group_id", "TEXT"),
        ] {
            let exists = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .and_then(|mut s| {
                    s.query_map([], |r| r.get::<_, String>(1))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(|e| e.to_string())?
                .iter()
                .any(|name| name == column);
            if !exists {
                connection
                    .execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN {column} {definition};"
                    ))
                    .map_err(|e| e.to_string())?;
            }
        }
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(2,?1)",
                params![now()],
            )
            .map_err(|e| e.to_string())?;
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(3,?1)",
                params![now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
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
             SELECT s.id,'scan' kind,NULL mode,s.status,s.started_at,s.finished_at,NULL parent_id,s.id root_scan_id,
                    (SELECT COUNT(*) FROM scan_items i WHERE i.scan_id=s.id) success,0 skipped,s.warning_count failed
             FROM scan_runs s
             UNION ALL
             SELECT p.id,'plan',NULL,p.status,p.started_at,p.finished_at,p.scan_id,p.scan_id,
                    (SELECT COUNT(*) FROM plan_items i WHERE i.plan_id=p.id),p.conflict_count,p.risk_count
             FROM plan_runs p
             UNION ALL
             SELECT e.id,'apply',e.mode,e.status,e.started_at,e.finished_at,e.plan_id,p.scan_id,
                    e.success_count,e.skipped_count,e.failed_count
             FROM execution_runs e JOIN plan_runs p ON p.id=e.plan_id
             UNION ALL
             SELECT v.id,'verify',NULL,v.status,v.started_at,v.finished_at,v.execution_id,p.scan_id,
                    v.success_count,0,v.failed_count
             FROM verify_runs v JOIN execution_runs e ON e.id=v.execution_id JOIN plan_runs p ON p.id=e.plan_id
             UNION ALL
             SELECT r.id,'rollback',r.mode,r.status,r.started_at,r.finished_at,r.execution_id,p.scan_id,
                    r.success_count,r.skipped_count,r.failed_count
             FROM rollback_runs r JOIN execution_runs e ON e.id=r.execution_id JOIN plan_runs p ON p.id=e.plan_id)
             SELECT id,kind,mode,status,started_at,finished_at,parent_id,root_scan_id,success,skipped,failed
             FROM history
             WHERE (?1 IS NULL OR kind=?1) AND (?2 IS NULL OR status=?2)
               AND (?3 IS NULL OR lower(id) LIKE '%' || lower(?3) || '%') AND {comparison}
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
                        status: r.get(3)?,
                        started_at: r.get(4)?,
                        finished_at: r.get(5)?,
                        parent_id: r.get(6)?,
                        root_scan_id: r.get(7)?,
                        success: r.get::<_, i64>(8)? as u64,
                        skipped: r.get::<_, i64>(9)? as u64,
                        failed: r.get::<_, i64>(10)? as u64,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
    pub fn get_run_detail(&self, kind: &str, id: &str) -> Result<RunDetailRow, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let sql = match kind {
            "scan" => "SELECT id,status,NULL,(SELECT COUNT(*) FROM scan_items WHERE scan_id=scan_runs.id),0,warning_count FROM scan_runs WHERE id=?1",
            "plan" => "SELECT id,status,scan_id,(SELECT COUNT(*) FROM plan_items WHERE plan_id=plan_runs.id),conflict_count,risk_count FROM plan_runs WHERE id=?1",
            "apply" => "SELECT id,status,plan_id,success_count,skipped_count,failed_count FROM execution_runs WHERE id=?1",
            "verify" => "SELECT id,status,execution_id,success_count,0,failed_count FROM verify_runs WHERE id=?1",
            "rollback" => "SELECT id,status,execution_id,success_count,skipped_count,failed_count FROM rollback_runs WHERE id=?1",
            _ => return Err("invalid_run_kind".to_string()),
        };
        conn.query_row(sql, params![id], |row| {
            Ok(RunDetailRow {
                id: row.get(0)?,
                kind: kind.to_string(),
                status: row.get(1)?,
                parent_id: row.get(2)?,
                success: row.get::<_, i64>(3)? as u64,
                skipped: row.get::<_, i64>(4)? as u64,
                failed: row.get::<_, i64>(5)? as u64,
            })
        })
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "run_not_found".to_string())
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
        let mut stmt = conn.prepare("SELECT i.id,i.conflict_group_id,(SELECT COUNT(*) FROM plan_conflict_members m WHERE m.conflict_group_id=i.conflict_group_id),i.ordinal,i.source_path,i.target_path,i.action,i.risk,i.reason FROM plan_items i WHERE i.plan_id=?1 AND i.ordinal>?2 AND (?3='' OR i.source_path LIKE '%' || ?3 || '%' OR i.target_path LIKE '%' || ?3 || '%') AND (?4='' OR i.risk=?4) ORDER BY i.ordinal ASC LIMIT ?5").map_err(|e|e.to_string())?;
        let mut rows = stmt
            .query_map(
                params![plan_id, after, needle, wanted_risk, (page_size + 1) as i64],
                |r| {
                    Ok(PlanItemRow {
                        id: r.get(0)?,
                        conflict_group_id: r.get(1)?,
                        conflict_member_count: r.get::<_, i64>(2)? as u64,
                        ordinal: r.get::<_, i64>(3)? as u64,
                        source_path: r.get(4)?,
                        target_path: r.get(5)?,
                        action: r.get(6)?,
                        risk: r.get(7)?,
                        reason: r.get(8)?,
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
        let (id, kind, target_path, existing_target_path) = conn
            .query_row(
                "SELECT id,kind,target_path,existing_target_path FROM plan_conflict_groups WHERE id=?1 AND plan_id=?2",
                params![conflict_group_id, plan_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan_conflict_not_found".to_string())?;
        let mut statement = conn
            .prepare("SELECT i.id,i.ordinal,i.source_path FROM plan_conflict_members m JOIN plan_items i ON i.id=m.plan_item_id WHERE m.conflict_group_id=?1 ORDER BY i.ordinal,i.id")
            .map_err(|error| error.to_string())?;
        let members = statement
            .query_map(params![conflict_group_id], |row| {
                Ok(PlanConflictMemberRow {
                    item_id: row.get(0)?,
                    ordinal: row.get::<_, i64>(1)? as u64,
                    source_path: row.get(2)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(PlanConflictDetail {
            id,
            kind,
            target_path,
            existing_target_path,
            members,
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
        let mut stmt=conn.prepare("SELECT id,execution_id,sequence_no,source_path,target_path,action,result,error,created_at FROM operation_logs WHERE execution_id=?1 AND sequence_no>?2 AND (?3='' OR source_path LIKE '%'||?3||'%' OR target_path LIKE '%'||?3||'%' OR error LIKE '%'||?3||'%') AND (?4='' OR result=?4) ORDER BY sequence_no,id LIMIT ?5").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(
                params![execution_id, after, q, status, limit.min(500) as i64],
                |r| {
                    Ok(OperationLogRow {
                        id: r.get(0)?,
                        execution_id: r.get(1)?,
                        sequence_no: r.get::<_, i64>(2)? as u64,
                        source_path: r.get(3)?,
                        target_path: r.get(4)?,
                        action: r.get(5)?,
                        result: r.get(6)?,
                        error: r.get(7)?,
                        created_at: r.get(8)?,
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
        let running: Option<String> = match kind {
            "scan" => conn
                .query_row(
                    "SELECT id FROM scan_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "plan" => conn
                .query_row(
                    "SELECT id FROM plan_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "apply" => conn
                .query_row(
                    "SELECT id FROM execution_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "verify" => conn
                .query_row(
                    "SELECT id FROM verify_runs WHERE id=?1 AND status='running'",
                    params![id],
                    |r| r.get(0),
                )
                .optional(),
            "rollback" => conn
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
            let active: Option<i64> = conn
                .query_row(&sql, params![id], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if active.is_some() {
                return Err("dependent_running_run_cannot_be_deleted".into());
            }
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        match kind {
            "scan" => {
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM verify_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM rollback_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM verify_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM rollback_runs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM operation_logs WHERE execution_id IN (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans))", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_items WHERE plan_id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE scan_id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_runs WHERE id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
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
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_items WHERE plan_id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
                tx.execute("WITH RECURSIVE plans(id) AS (SELECT id FROM plan_runs WHERE id=?1 UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id) DELETE FROM plan_runs WHERE id IN (SELECT id FROM plans)", params![id]).map_err(|e|e.to_string())?;
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
        let seed = match kind { "scan" => "SELECT id FROM plan_runs WHERE scan_id=?1", "plan" => "SELECT id FROM plan_runs WHERE id=?1", "apply" => "SELECT plan_id FROM execution_runs WHERE id=?1", "verify" => "SELECT plan_id FROM execution_runs WHERE id=(SELECT execution_id FROM verify_runs WHERE id=?1)", "rollback" => "SELECT plan_id FROM execution_runs WHERE id=(SELECT execution_id FROM rollback_runs WHERE id=?1)", _ => return Err("invalid_run_kind".into()) };
        let sql = format!("WITH RECURSIVE plans(id) AS ({seed} UNION ALL SELECT p.id FROM plan_runs p JOIN plans q ON p.parent_plan_id=q.id), executions(id) AS (SELECT id FROM execution_runs WHERE plan_id IN (SELECT id FROM plans)) SELECT (SELECT COUNT(*) FROM plans),(SELECT COUNT(*) FROM executions),(SELECT COUNT(*) FROM operation_logs WHERE execution_id IN (SELECT id FROM executions)) + (SELECT COUNT(*) FROM verify_logs WHERE execution_id IN (SELECT id FROM executions)) + (SELECT COUNT(*) FROM rollback_logs WHERE execution_id IN (SELECT id FROM executions)), EXISTS(SELECT 1 FROM plan_runs WHERE id IN (SELECT id FROM plans) AND status='running') OR EXISTS(SELECT 1 FROM execution_runs WHERE id IN (SELECT id FROM executions) AND status='running') OR EXISTS(SELECT 1 FROM verify_runs WHERE execution_id IN (SELECT id FROM executions) AND status='running') OR EXISTS(SELECT 1 FROM rollback_runs WHERE execution_id IN (SELECT id FROM executions) AND status='running')");
        conn.query_row(&sql, params![id], |r| {
            Ok(HistoryCleanupPreview {
                plans: r.get::<_, i64>(0)? as u64,
                executions: r.get::<_, i64>(1)? as u64,
                logs: r.get::<_, i64>(2)? as u64,
                blocked: r.get::<_, i64>(3)? != 0,
            })
        })
        .map_err(|e| e.to_string())
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
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
impl ScanStore for SqliteScanStore {
    fn previous_metadata(
        &self,
        path: &Path,
        fp: &FileFingerprint,
    ) -> Result<Option<TrackMetadata>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let row: Option<String> = conn.query_row("SELECT metadata_json FROM library_files WHERE path=?1 AND size_bytes=?2 AND mtime_ns=?3 AND metadata_status='ok'", params![path.to_string_lossy(), fp.size_bytes as i64, fp.mtime_ns.to_string()], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        row.map(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
            .transpose()
    }
    fn begin_scan(&self, source: &Path) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO scan_runs(id,source_root,status,started_at) VALUES(?1,?2,'running',?3)",params![id,source.to_string_lossy(),now()]).map_err(|e|e.to_string())?;
        Ok(id)
    }
    fn save_batch(&self, scan_id: &str, files: &[ScannedFile]) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for f in files {
            let json = f
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| e.to_string())?;
            tx.execute("INSERT INTO library_files(path,size_bytes,mtime_ns,metadata_json,metadata_status,kind,last_seen_scan_id) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(path) DO UPDATE SET size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,metadata_json=excluded.metadata_json,metadata_status=excluded.metadata_status,kind=excluded.kind,last_seen_scan_id=excluded.last_seen_scan_id",params![f.path.to_string_lossy(),f.fingerprint.size_bytes as i64,f.fingerprint.mtime_ns.to_string(),json,if f.metadata.is_some(){"ok"}else{"error"},if f.kind == FileKind::Image {"image"} else {"music"},scan_id]).map_err(|e|e.to_string())?;
            tx.execute(
                "INSERT OR IGNORE INTO scan_items(scan_id,path) VALUES(?1,?2)",
                params![scan_id, f.path.to_string_lossy()],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
    fn finish_scan(&self, scan_id: &str, status: &str, warnings: u64) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .execute(
                "UPDATE scan_runs SET status=?2,finished_at=?3,warning_count=?4 WHERE id=?1",
                params![scan_id, status, now(), warnings as i64],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn save_scan_warning(&self, scan_id: &str, warning: &str) -> Result<(), String> {
        self.connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?
            .execute(
                "INSERT INTO scan_warnings(id,scan_id,warning,created_at) VALUES(?1,?2,?3,?4)",
                params![Uuid::new_v4().to_string(), scan_id, warning, now()],
            )
            .map_err(|error| error.to_string())?;
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
        let mut statement = conn.prepare("SELECT f.path,f.size_bytes,f.mtime_ns,f.metadata_json,f.kind FROM scan_items s JOIN library_files f ON f.path=s.path WHERE s.scan_id=?1 ORDER BY f.path").map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![scan_id], |row| {
                let metadata_json: Option<String> = row.get(3)?;
                Ok(ScannedFile {
                    id: Uuid::new_v4(),
                    path: row.get::<_, String>(0)?.into(),
                    fingerprint: FileFingerprint {
                        size_bytes: row.get::<_, i64>(1)? as u64,
                        mtime_ns: row.get::<_, String>(2)?.parse().unwrap_or_default(),
                    },
                    metadata: metadata_json.and_then(|json| serde_json::from_str(&json).ok()),
                    kind: if row.get::<_, String>(4)? == "image" {
                        FileKind::Image
                    } else {
                        FileKind::Music
                    },
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn begin_plan(
        &self,
        scan_id: &str,
        target_root: &Path,
        naming: &music_folder_core::NamingRules,
    ) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let rules = serde_json::to_string(naming).map_err(|e| e.to_string())?;
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO plan_runs(id,scan_id,target_root,rules_json,status,started_at) VALUES(?1,?2,?3,?4,'running',?5)", params![id,scan_id,target_root.to_string_lossy(),rules,now()]).map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn save_plan_items(&self, plan_id: &str, items: &[PlanItem]) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        for item in items {
            let group_id = item.conflict_group_id.map(|value| value.to_string());
            let target = item
                .target
                .as_ref()
                .map(|value| value.to_string_lossy().into_owned());
            tx.execute("INSERT INTO plan_items(id,plan_id,ordinal,source_path,target_path,conflict_group_id,action,risk,reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![item.id.to_string(),plan_id,item.ordinal as i64,item.file.path.to_string_lossy(),target,group_id,plan_action_name(item.action),risk_name(item.risk),item.reason]).map_err(|error| error.to_string())?;
            if let (Some(group_id), Some(target)) = (&group_id, &target) {
                tx.execute("INSERT OR IGNORE INTO plan_conflict_groups(id,plan_id,kind,normalized_target_path,target_path) VALUES(?1,?2,'plan_items',?3,?4)", params![group_id,plan_id,target.to_lowercase(),target]).map_err(|error| error.to_string())?;
                tx.execute("INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id) VALUES(?1,?2)", params![group_id,item.id.to_string()]).map_err(|error| error.to_string())?;
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
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("UPDATE plan_runs SET status='completed',finished_at=?2,conflict_count=?3,risk_count=?4,snapshot_hash=?5 WHERE id=?1",params![plan_id,now(),conflict_count as i64,risk_count as i64,snapshot_hash]).map_err(|error| error.to_string())?;
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

impl PlanRevisionStore for SqliteScanStore {
    fn revise_plan(
        &self,
        parent_plan_id: &str,
        changes: &[ManualTargetChange],
    ) -> Result<String, String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let _parent: (String, String) = conn
            .query_row(
                "SELECT scan_id,target_root FROM plan_runs WHERE id=?1 AND status='completed'",
                params![parent_plan_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "parent_plan_not_completed".to_string())?;
        let mut source = conn.prepare("SELECT id,ordinal,source_path,target_path,action,risk,reason FROM plan_items WHERE plan_id=?1 ORDER BY ordinal").map_err(|e|e.to_string())?;
        let rows = source
            .query_map(params![parent_plan_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(source);
        let overrides: std::collections::HashMap<_, _> = changes
            .iter()
            .map(|c| (c.plan_item_id.as_str(), c))
            .collect();
        let id = Uuid::new_v4().to_string();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO plan_runs(id,scan_id,parent_plan_id,target_root,rules_json,status,started_at) SELECT ?1,scan_id,id,target_root,rules_json,'running',?2 FROM plan_runs WHERE id=?3", params![id,now(),parent_plan_id]).map_err(|e|e.to_string())?;
        let mut revised = Vec::with_capacity(rows.len());
        for (old_id, ordinal, source_path, old_target, mut action, mut risk, mut reason) in rows {
            let (target, origin) = if let Some(change) = overrides.get(old_id.as_str()) {
                let target = change.target.to_string_lossy().into_owned();
                if target == source_path || assess_windows_path(&change.target).is_err() {
                    action = "skip".into();
                    risk = "invalid_target".into();
                    reason = Some(change.reason.clone());
                } else {
                    action = "move".into();
                    risk = "none".into();
                    reason = Some(change.reason.clone());
                }
                (Some(target), "manual")
            } else {
                if risk == "conflict" && reason.as_deref() == Some("target_conflict") {
                    action = "move".into();
                    risk = "none".into();
                    reason = None;
                }
                (old_target, "rule")
            };
            revised.push((
                Uuid::new_v4().to_string(),
                ordinal,
                source_path,
                target,
                origin,
                action,
                risk,
                reason,
            ));
        }
        let mut target_counts = std::collections::HashMap::<String, usize>::new();
        for (_, _, _, target, _, action, _, _) in &revised {
            if action == "move" {
                if let Some(target) = target {
                    *target_counts.entry(target.to_lowercase()).or_default() += 1;
                }
            }
        }
        let conflict_groups: std::collections::HashMap<String, String> = target_counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(target, _)| (target, Uuid::new_v4().to_string()))
            .collect();
        let mut digest = Sha256::new();
        let mut conflicts = 0u64;
        let mut risks = 0u64;
        for (new_item, ordinal, source_path, target, origin, mut action, mut risk, mut reason) in
            revised
        {
            let conflict_group_id = target
                .as_ref()
                .and_then(|target| conflict_groups.get(&target.to_lowercase()))
                .cloned();
            if conflict_group_id.is_some() {
                action = "skip".into();
                risk = "conflict".into();
                reason = Some("target_conflict".into());
                conflicts += 1;
            }
            if risk != "none" {
                risks += 1;
            }
            tx.execute("INSERT INTO plan_items(id,plan_id,ordinal,source_path,target_path,target_origin,conflict_group_id,action,risk,reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![new_item,id,ordinal,source_path,target,origin,conflict_group_id,action,risk,reason]).map_err(|e|e.to_string())?;
            if let (Some(group_id), Some(target)) = (&conflict_group_id, &target) {
                tx.execute("INSERT OR IGNORE INTO plan_conflict_groups(id,plan_id,kind,normalized_target_path,target_path) VALUES(?1,?2,'plan_items',?3,?4)", params![group_id,id,target.to_lowercase(),target]).map_err(|e|e.to_string())?;
                tx.execute("INSERT INTO plan_conflict_members(conflict_group_id,plan_item_id) VALUES(?1,?2)", params![group_id,new_item]).map_err(|e|e.to_string())?;
            }
            digest.update((ordinal as u64).to_le_bytes());
            digest.update(source_path.as_bytes());
            digest.update([0]);
            if let Some(t) = target {
                digest.update(t.as_bytes())
            };
            digest.update([0]);
            digest.update(action.as_bytes());
            digest.update([0]);
            digest.update(risk.as_bytes());
            digest.update([0]);
            if let Some(r) = reason {
                digest.update(r.as_bytes())
            };
            digest.update([0xff]);
        }
        tx.execute("UPDATE plan_runs SET status='completed',finished_at=?2,conflict_count=?3,risk_count=?4,snapshot_hash=?5 WHERE id=?1",params![id,now(),conflicts as i64,risks as i64,format!("{:x}",digest.finalize())]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(id)
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

impl ApplyStore for SqliteScanStore {
    fn load_completed_plan(&self, plan_id: &str) -> Result<Vec<ApplyItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let status: Option<String> = conn
            .query_row(
                "SELECT id FROM plan_runs WHERE id=?1 AND status='completed'",
                params![plan_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if status.is_none() {
            return Err("plan_not_completed".into());
        }
        let mut statement = conn.prepare("SELECT id,ordinal,source_path,target_path,action,risk,reason FROM plan_items WHERE plan_id=?1 ORDER BY ordinal").map_err(|e| e.to_string())?;
        let items = statement
            .query_map(params![plan_id], |r| {
                Ok(ApplyItem {
                    plan_item_id: r.get(0)?,
                    ordinal: r.get::<_, i64>(1)? as u64,
                    source: r.get::<_, String>(2)?.into(),
                    target: r.get::<_, Option<String>>(3)?.map(Into::into),
                    action: if r.get::<_, String>(4)? == "move" {
                        PlanAction::Move
                    } else {
                        PlanAction::Skip
                    },
                    risk: match r.get::<_, String>(5)?.as_str() {
                        "none" => Risk::None,
                        "conflict" => Risk::Conflict,
                        "metadata_missing" => Risk::MetadataMissing,
                        "path_too_long" => Risk::PathTooLong,
                        _ => Risk::InvalidTarget,
                    },
                    reason: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(items)
    }
    fn validate_plan_snapshot(&self, plan_id: &str) -> Result<(), String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
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
        let mut stmt = conn.prepare("SELECT ordinal,source_path,target_path,action,risk,reason FROM plan_items WHERE plan_id=?1 ORDER BY ordinal").map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let rows = stmt
            .query_map(params![plan_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (ordinal, source, target, action, risk, reason) = row.map_err(|e| e.to_string())?;
            digest.update((ordinal as u64).to_le_bytes());
            digest.update(source.as_bytes());
            digest.update([0]);
            if let Some(value) = target {
                digest.update(value.as_bytes());
            }
            digest.update([0]);
            digest.update(action.as_bytes());
            digest.update([0]);
            digest.update(risk.as_bytes());
            digest.update([0]);
            if let Some(value) = reason {
                digest.update(value.as_bytes());
            }
            digest.update([0xff]);
        }
        if format!("{:x}", digest.finalize()) != stored {
            return Err("plan_snapshot_mismatch".into());
        }
        Ok(())
    }
    fn successful_plan_item_ids(&self, plan_id: &str) -> Result<Vec<String>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut stmt = conn.prepare("SELECT DISTINCT o.plan_item_id FROM operation_logs o JOIN execution_runs e ON e.id=o.execution_id WHERE e.plan_id=?1 AND e.mode='apply' AND o.result='success' AND o.source_deleted=1").map_err(|e| e.to_string())?;
        let ids = stmt
            .query_map(params![plan_id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(ids)
    }
    fn begin_execution(&self, plan_id: &str, dry_run: bool) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        self.connection.lock().map_err(|_|"database mutex poisoned".to_string())?.execute("INSERT INTO execution_runs(id,plan_id,mode,status,started_at) VALUES(?1,?2,?3,'running',?4)",params![id,plan_id,if dry_run{"dry_run"}else{"apply"},now()]).map_err(|e|e.to_string())?;
        Ok(id)
    }
    fn save_operation(&self, execution_id: &str, op: &OperationLog) -> Result<(), String> {
        self.connection.lock().map_err(|_|"database mutex poisoned".to_string())?.execute("INSERT INTO operation_logs(id,execution_id,plan_item_id,sequence_no,source_path,target_path,action,result,error,source_deleted,expected_size,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![Uuid::new_v4().to_string(),execution_id,op.plan_item_id,op.sequence_no as i64,op.source.to_string_lossy(),op.target.as_ref().map(|p|p.to_string_lossy().into_owned()),op.action,op.result,op.error,op.source_deleted as i32,op.expected_size.map(|size|size as i64),now()]).map_err(|e|e.to_string())?;
        Ok(())
    }
    fn finish_execution(
        &self,
        execution_id: &str,
        status: &str,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_|"database mutex poisoned".to_string())?.execute("UPDATE execution_runs SET status=?2,finished_at=?3,success_count=?4,skipped_count=?5,failed_count=?6 WHERE id=?1",params![execution_id,status,now(),success as i64,skipped as i64,failed as i64]).map_err(|e|e.to_string())?;
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

impl VerifyStore for SqliteScanStore {
    fn begin_verify(&self, execution_id: &str) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO verify_runs(id,execution_id,status,started_at) VALUES(?1,?2,'running',?3)", params![id,execution_id,now()]).map_err(|e|e.to_string())?;
        Ok(id)
    }
    fn load_successful_operations(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let mut stmt = conn.prepare("SELECT id,sequence_no,source_path,target_path,action,expected_size FROM operation_logs WHERE execution_id=?1 AND result='success' AND action IN ('move','copy_delete') ORDER BY sequence_no").map_err(|e| e.to_string())?;
        let items = stmt
            .query_map(params![execution_id], |row| {
                Ok(VerifyItem {
                    operation_id: row.get(0)?,
                    sequence_no: row.get::<_, i64>(1)? as u64,
                    source: row.get::<_, String>(2)?.into(),
                    target: row.get::<_, Option<String>>(3)?.map(Into::into),
                    action: row.get(4)?,
                    expected_size: row.get::<_, Option<i64>>(5)?.map(|size| size as u64),
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(items)
    }
    fn save_verify_result(
        &self,
        execution_id: &str,
        operation_id: &str,
        result: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO verify_logs(id,execution_id,operation_id,result,error,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![Uuid::new_v4().to_string(),execution_id,operation_id,result,error,now()]).map_err(|e|e.to_string())?;
        Ok(())
    }
    fn finish_verify(
        &self,
        verify_id: &str,
        status: &str,
        success: u64,
        failed: u64,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("UPDATE verify_runs SET status=?2,finished_at=?3,success_count=?4,failed_count=?5 WHERE id=?1", params![verify_id,status,now(),success as i64,failed as i64]).map_err(|e|e.to_string())?;
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

impl RollbackStore for SqliteScanStore {
    fn begin_rollback(&self, execution_id: &str, dry_run: bool) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO rollback_runs(id,execution_id,mode,status,started_at) VALUES(?1,?2,?3,'running',?4)", params![id,execution_id,if dry_run {"dry_run"} else {"rollback"},now()]).map_err(|e|e.to_string())?;
        Ok(id)
    }
    fn load_rollback_items(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String> {
        <Self as VerifyStore>::load_successful_operations(self, execution_id)
    }
    fn save_rollback_result(
        &self,
        execution_id: &str,
        operation_id: &str,
        result: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO rollback_logs(id,execution_id,operation_id,result,error,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![Uuid::new_v4().to_string(),execution_id,operation_id,result,error,now()]).map_err(|e|e.to_string())?;
        Ok(())
    }
    fn finish_rollback(
        &self,
        rollback_id: &str,
        status: &str,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String> {
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("UPDATE rollback_runs SET status=?2,finished_at=?3,success_count=?4,skipped_count=?5,failed_count=?6 WHERE id=?1",params![rollback_id,status,now(),success as i64,skipped as i64,failed as i64]).map_err(|e|e.to_string())?;
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
