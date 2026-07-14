use music_folder_core::{
    ports::{ApplyStore, PlanStore, RollbackStore, ScanStore, VerifyStore},
    ApplyItem, FileFingerprint, OperationLog, PlanAction, PlanItem, Risk, ScannedFile,
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

#[derive(serde::Serialize)]
pub struct HistoryRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub started_at: i64,
}
#[derive(serde::Serialize)]
pub struct PlanItemRow {
    pub id: String,
    pub ordinal: u64,
    pub source_path: String,
    pub target_path: Option<String>,
    pub action: String,
    pub risk: String,
    pub reason: Option<String>,
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

pub struct SqliteScanStore {
    connection: Mutex<Connection>,
}
impl SqliteScanStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(|e| e.to_string())?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS scan_runs (id TEXT PRIMARY KEY, source_root TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, warning_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS library_files (path TEXT PRIMARY KEY, size_bytes INTEGER NOT NULL, mtime_ns TEXT NOT NULL, metadata_json TEXT, metadata_status TEXT NOT NULL, last_seen_scan_id TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS scan_items (scan_id TEXT NOT NULL REFERENCES scan_runs(id), path TEXT NOT NULL, PRIMARY KEY(scan_id,path));
          CREATE TABLE IF NOT EXISTS scan_warnings (id TEXT PRIMARY KEY, scan_id TEXT NOT NULL REFERENCES scan_runs(id), warning TEXT NOT NULL, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS plan_runs (id TEXT PRIMARY KEY, scan_id TEXT NOT NULL REFERENCES scan_runs(id), target_root TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, conflict_count INTEGER NOT NULL DEFAULT 0, risk_count INTEGER NOT NULL DEFAULT 0, snapshot_hash TEXT);
          CREATE TABLE IF NOT EXISTS plan_items (id TEXT PRIMARY KEY, plan_id TEXT NOT NULL REFERENCES plan_runs(id), ordinal INTEGER NOT NULL, source_path TEXT NOT NULL, target_path TEXT, action TEXT NOT NULL, risk TEXT NOT NULL, reason TEXT);
          CREATE TABLE IF NOT EXISTS execution_runs (id TEXT PRIMARY KEY, plan_id TEXT NOT NULL REFERENCES plan_runs(id), mode TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, success_count INTEGER NOT NULL DEFAULT 0, skipped_count INTEGER NOT NULL DEFAULT 0, failed_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS operation_logs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), plan_item_id TEXT NOT NULL REFERENCES plan_items(id), sequence_no INTEGER NOT NULL, source_path TEXT NOT NULL, target_path TEXT, action TEXT NOT NULL, result TEXT NOT NULL, error TEXT, source_deleted INTEGER NOT NULL, expected_size INTEGER, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS verify_logs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), operation_id TEXT NOT NULL REFERENCES operation_logs(id), result TEXT NOT NULL, error TEXT, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS rollback_logs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), operation_id TEXT NOT NULL REFERENCES operation_logs(id), result TEXT NOT NULL, error TEXT, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS verify_runs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, success_count INTEGER NOT NULL DEFAULT 0, failed_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS rollback_runs (id TEXT PRIMARY KEY, execution_id TEXT NOT NULL REFERENCES execution_runs(id), mode TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER, success_count INTEGER NOT NULL DEFAULT 0, skipped_count INTEGER NOT NULL DEFAULT 0, failed_count INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS run_metrics (run_id TEXT NOT NULL, phase TEXT NOT NULL, elapsed_ms INTEGER NOT NULL, item_count INTEGER NOT NULL DEFAULT 0);
          CREATE INDEX IF NOT EXISTS idx_scan_items_scan ON scan_items(scan_id);
          CREATE INDEX IF NOT EXISTS idx_plan_items_plan ON plan_items(plan_id, ordinal);
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
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(2,?1)",
                params![now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn list_history(&self, limit: u32, cursor: Option<i64>) -> Result<Vec<HistoryRow>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let after = cursor.unwrap_or(i64::MAX);
        let mut stmt = conn.prepare("SELECT id,'scan',status,started_at FROM scan_runs WHERE started_at<?1 UNION ALL SELECT id,'plan',status,started_at FROM plan_runs WHERE started_at<?1 UNION ALL SELECT id,'apply',status,started_at FROM execution_runs WHERE started_at<?1 UNION ALL SELECT id,'verify',status,started_at FROM verify_runs WHERE started_at<?1 UNION ALL SELECT id,'rollback',status,started_at FROM rollback_runs WHERE started_at<?1 ORDER BY started_at DESC,id DESC LIMIT ?2").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(params![after, limit], |r| {
                Ok(HistoryRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    status: r.get(2)?,
                    started_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
    /// Keyset pagination: callers retain the last ordinal; no OFFSET scan or full result transfer.
    pub fn list_plan_items(
        &self,
        plan_id: &str,
        after_ordinal: Option<u64>,
        limit: u32,
        query: Option<&str>,
        risk: Option<&str>,
    ) -> Result<Vec<PlanItemRow>, String> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let after = after_ordinal.unwrap_or(0) as i64;
        let needle = query.unwrap_or("");
        let wanted_risk = risk.unwrap_or("");
        let mut stmt = conn.prepare("SELECT id,ordinal,source_path,target_path,action,risk,reason FROM plan_items WHERE plan_id=?1 AND ordinal>?2 AND (?3='' OR source_path LIKE '%' || ?3 || '%' OR target_path LIKE '%' || ?3 || '%') AND (?4='' OR risk=?4) ORDER BY ordinal ASC LIMIT ?5").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map(
                params![plan_id, after, needle, wanted_risk, limit.min(500) as i64],
                |r| {
                    Ok(PlanItemRow {
                        id: r.get(0)?,
                        ordinal: r.get::<_, i64>(1)? as u64,
                        source_path: r.get(2)?,
                        target_path: r.get(3)?,
                        action: r.get(4)?,
                        risk: r.get(5)?,
                        reason: r.get(6)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
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
            tx.execute("INSERT INTO library_files(path,size_bytes,mtime_ns,metadata_json,metadata_status,last_seen_scan_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(path) DO UPDATE SET size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,metadata_json=excluded.metadata_json,metadata_status=excluded.metadata_status,last_seen_scan_id=excluded.last_seen_scan_id",params![f.path.to_string_lossy(),f.fingerprint.size_bytes as i64,f.fingerprint.mtime_ns.to_string(),json,if f.metadata.is_some(){"ok"}else{"error"},scan_id]).map_err(|e|e.to_string())?;
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
        let mut statement = conn.prepare("SELECT f.path,f.size_bytes,f.mtime_ns,f.metadata_json FROM scan_items s JOIN library_files f ON f.path=s.path WHERE s.scan_id=?1 ORDER BY f.path").map_err(|error| error.to_string())?;
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
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn begin_plan(&self, scan_id: &str, target_root: &Path) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        self.connection.lock().map_err(|_| "database mutex poisoned".to_string())?.execute("INSERT INTO plan_runs(id,scan_id,target_root,status,started_at) VALUES(?1,?2,?3,'running',?4)", params![id,scan_id,target_root.to_string_lossy(),now()]).map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn save_plan_items(&self, plan_id: &str, items: &[PlanItem]) -> Result<(), String> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| "database mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        for item in items {
            tx.execute("INSERT INTO plan_items(id,plan_id,ordinal,source_path,target_path,action,risk,reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![item.id.to_string(),plan_id,item.ordinal as i64,item.file.path.to_string_lossy(),item.target.as_ref().map(|value| value.to_string_lossy().into_owned()),plan_action_name(item.action),risk_name(item.risk),item.reason]).map_err(|error| error.to_string())?;
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
