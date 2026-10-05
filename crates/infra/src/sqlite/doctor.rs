use super::*;
use music_folder_core::doctor::{DoctorIssue, DoctorRun, DoctorStore, RULE_VERSION};

impl DoctorStore for SqliteScanStore {
    fn begin_doctor(&self, scan_id: &str) -> Result<DoctorRun, String> {
        let run = DoctorRun {
            id: Uuid::new_v4().to_string(),
            scan_id: scan_id.into(),
            status: RunStatus::Running,
            rule_version: RULE_VERSION,
            files: 0,
            cache_hits: 0,
            failures: 0,
            issue_count: 0,
            error: None,
        };
        self.connection.lock().map_err(|e| e.to_string())?.execute(
            "INSERT INTO doctor_runs(id,scan_id,status,rule_version,summary_json,started_at) VALUES(?1,?2,'running',?3,?4,?5)",
            params![run.id, scan_id, RULE_VERSION, serde_json::to_string(&run).map_err(|e| e.to_string())?, now()],
        ).map_err(|e| e.to_string())?;
        Ok(run)
    }

    fn doctor_files(&self, scan_id: &str) -> Result<Vec<ScannedFile>, String> {
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let mut files = load_scan_snapshot_items(&conn, scan_id)?;
        for file in &mut files {
            // Stable diagnostic reference without altering existing scan/Plan identifiers.
            let (encoding, raw) = encode_path(&file.path);
            let mut hash = Sha256::new();
            hash.update(scan_id.as_bytes());
            hash.update(encoding.as_bytes());
            hash.update(raw);
            let digest = hash.finalize();
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&digest[..16]);
            file.id = Uuid::from_bytes(bytes);
        }
        Ok(files)
    }

    fn doctor_warnings(&self, scan_id: &str) -> Result<Vec<String>, String> {
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let mut statement = conn
            .prepare("SELECT warning FROM scan_warnings WHERE scan_id=?1 ORDER BY created_at,id")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![scan_id], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    fn finish_doctor(&self, run: &DoctorRun, issues: &[DoctorIssue]) -> Result<(), String> {
        let mut conn = self.connection.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let changed = tx.execute("UPDATE doctor_runs SET status=?2,summary_json=?3,finished_at=?4 WHERE id=?1 AND status='running'",
            params![run.id, run.status.as_str(), serde_json::to_string(run).map_err(|e| e.to_string())?, now()]).map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err("doctor_not_running".into());
        }
        for (ordinal, issue) in issues.iter().enumerate() {
            let severity = serde_json::to_value(issue.severity).map_err(|e| e.to_string())?;
            tx.execute("INSERT INTO doctor_issues(run_id,ordinal,code,severity,category,issue_json) VALUES(?1,?2,?3,?4,?5,?6)",
                params![run.id, ordinal as i64, issue.code, severity.as_str(), issue.category,
                    serde_json::to_string(issue).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

impl SqliteScanStore {
    pub fn doctor_id_for_scan(&self, scan_id: &str) -> Result<String, String> {
        self.connection
            .lock()
            .map_err(|e| e.to_string())?
            .query_row(
                "SELECT id FROM doctor_runs WHERE scan_id=?1",
                params![scan_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub fn doctor_source(&self, id: &str) -> Result<PathBuf, String> {
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let (encoding, raw): (String, Vec<u8>) = conn.query_row(
            "SELECT s.source_root_encoding,s.source_root_blob FROM doctor_runs d JOIN scan_runs s ON s.id=d.scan_id WHERE d.id=?1",
            params![id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).map_err(|e| e.to_string())?;
        decode_path(&encoding, &raw)
    }

    pub fn doctor_history(
        &self,
        cursor: usize,
    ) -> Result<crate::doctor_view::Page<crate::doctor_view::DoctorHistory>, String> {
        use crate::doctor_view::{DoctorHistory, Page};
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM doctor_runs", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare("SELECT d.summary_json,d.started_at,d.finished_at,s.source_root_encoding,s.source_root_blob FROM doctor_runs d JOIN scan_runs s ON s.id=d.scan_id ORDER BY d.started_at DESC,d.id DESC LIMIT 50 OFFSET ?1").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![i64::try_from(cursor).map_err(|_| "doctor_cursor_invalid")?],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            let (json, started_at, finished_at, encoding, raw) = row.map_err(|e| e.to_string())?;
            items.push(DoctorHistory {
                run: serde_json::from_str(&json).map_err(|e| e.to_string())?,
                source: path_envelope(&decode_path(&encoding, &raw)?, "source"),
                started_at,
                finished_at,
            });
        }
        let end = cursor.saturating_add(items.len());
        Ok(Page {
            items,
            total: total as usize,
            next_cursor: (end < total as usize).then_some(end),
        })
    }

    pub fn doctor_history_entry(
        &self,
        id: &str,
    ) -> Result<crate::doctor_view::DoctorHistory, String> {
        let run = self.doctor_run(id)?;
        let source = path_envelope(&self.doctor_source(id)?, "source");
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let (started_at, finished_at) = conn
            .query_row(
                "SELECT started_at,finished_at FROM doctor_runs WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        Ok(crate::doctor_view::DoctorHistory {
            run,
            source,
            started_at,
            finished_at,
        })
    }

    pub fn doctor_issue_page(
        &self,
        id: &str,
        cursor: usize,
        severity: Option<&str>,
        code: Option<&str>,
        category: Option<&str>,
    ) -> Result<crate::doctor_view::Page<crate::doctor_view::IssueRow>, String> {
        use crate::doctor_view::{IssueRow, Page};
        self.doctor_run(id)?;
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM doctor_issues WHERE run_id=?1 AND (?2 IS NULL OR severity=?2) AND (?3 IS NULL OR code=?3) AND (?4 IS NULL OR category=?4)", params![id, severity, code, category], |r| r.get(0)).map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare("SELECT ordinal,code,severity,category,json_array_length(issue_json,'$.file_ids') FROM doctor_issues WHERE run_id=?1 AND (?2 IS NULL OR severity=?2) AND (?3 IS NULL OR code=?3) AND (?4 IS NULL OR category=?4) ORDER BY ordinal LIMIT 100 OFFSET ?5").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    id,
                    severity,
                    code,
                    category,
                    i64::try_from(cursor).map_err(|_| "doctor_cursor_invalid")?
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            let (ordinal, code, severity, category, file_count) = row.map_err(|e| e.to_string())?;
            items.push(IssueRow {
                ordinal: ordinal as usize,
                code,
                severity: serde_json::from_value(serde_json::Value::String(severity))
                    .map_err(|e| e.to_string())?,
                category,
                file_count: file_count as usize,
            });
        }
        let end = cursor.saturating_add(items.len());
        Ok(Page {
            items,
            total: total as usize,
            next_cursor: (end < total as usize).then_some(end),
        })
    }

    pub fn doctor_run(&self, id: &str) -> Result<DoctorRun, String> {
        let json: Option<String> = self
            .connection
            .lock()
            .map_err(|e| e.to_string())?
            .query_row(
                "SELECT summary_json FROM doctor_runs WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        serde_json::from_str(&json.ok_or("doctor_run_not_found")?).map_err(|e| e.to_string())
    }

    pub fn doctor_issues(
        &self,
        id: &str,
        severity: Option<&str>,
        code: Option<&str>,
        category: Option<&str>,
    ) -> Result<Vec<DoctorIssue>, String> {
        self.doctor_run(id)?;
        let conn = self.connection.lock().map_err(|e| e.to_string())?;
        let mut statement = conn.prepare("SELECT issue_json FROM doctor_issues WHERE run_id=?1 AND (?2 IS NULL OR severity=?2) AND (?3 IS NULL OR code=?3) AND (?4 IS NULL OR category=?4) ORDER BY ordinal").map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![id, severity, code, category], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| e.to_string())?;
        rows.map(|r| {
            serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
    }
}
