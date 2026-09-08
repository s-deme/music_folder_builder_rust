use music_folder_core::{ports::ScanStore, RunStatus};
use music_folder_infra::sqlite::{
    DiagnosticEventInput, DiagnosticRetentionPolicy, SqliteScanStore,
};
use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::tempdir;

fn event(class: &str, run_id: &str, code: &str) -> DiagnosticEventInput {
    DiagnosticEventInput {
        run_id: Some(run_id.to_owned()),
        attempt_id: None,
        class: class.to_owned(),
        severity: "warning".into(),
        phase: "verify".into(),
        code: code.to_owned(),
        item_id: None,
        sequence_no: None,
        path_role: Some("source".into()),
        message_key: code.to_owned(),
        payload: json!({"detail": code}),
        contains_sensitive_path: false,
        protected: false,
    }
}

#[test]
fn retention_is_bounded_and_preserves_recovery_evidence_until_archive() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("diagnostics.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = store.begin_scan(&temporary.path().join("source")).unwrap();
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let progress_id = store
        .record_diagnostic(event("progress", &scan_id, "old_progress"))
        .unwrap();
    let diagnostic_id = store
        .record_diagnostic(event("diagnostic", &scan_id, "old_diagnostic"))
        .unwrap();
    let recovery_id = store
        .record_diagnostic(event("recovery", &scan_id, "old_recovery"))
        .unwrap();
    let mut protected = event("audit", &scan_id, "legal_hold_audit");
    protected.protected = true;
    let protected_id = store.record_diagnostic(protected).unwrap();
    let reference_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let old = reference_time - 365 * 24 * 60 * 60;
    let external = Connection::open(&database).unwrap();
    for id in [&progress_id, &diagnostic_id, &recovery_id, &protected_id] {
        external
            .execute(
                "UPDATE diagnostic_events SET created_at=?2 WHERE id=?1",
                params![id, old],
            )
            .unwrap();
    }
    drop(external);

    let first = store
        .run_diagnostic_retention_at(&DiagnosticRetentionPolicy::default(), reference_time)
        .unwrap();
    assert_eq!(first.deleted_progress_debug, 1);
    assert_eq!(first.deleted_diagnostic, 1);
    assert_eq!(first.deleted_recovery_audit, 0);
    assert!(first.protected_remaining >= 2);

    store
        .archive_history("scan", &scan_id, Some(&temporary.path().join("archives")))
        .unwrap();
    let second = store
        .run_diagnostic_retention_at(&DiagnosticRetentionPolicy::default(), reference_time)
        .unwrap();
    assert_eq!(second.deleted_recovery_audit, 1);
    let remaining: i64 = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM diagnostic_events WHERE id=?1",
            params![protected_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);

    let unsafe_policy = DiagnosticRetentionPolicy {
        progress_debug_days: 1,
        ..DiagnosticRetentionPolicy::default()
    };
    assert_eq!(
        store
            .run_diagnostic_retention_at(&unsafe_policy, reference_time)
            .unwrap_err(),
        "diagnostic_retention_below_safety_minimum"
    );
}

#[test]
fn diagnostic_export_redacts_sensitive_paths_and_secret_fields_and_verifies_digest() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("export.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let mut sensitive = event("diagnostic", "scan-correlation", "source_read_failed");
    sensitive.contains_sensitive_path = true;
    sensitive.payload = json!({
        "source_path": r"C:\\Users\\Person\\Private\\track.mp3",
        "access_token": "must-never-leak",
        "cause": {"password": "also-secret"},
        "message": "request failed with Bearer bearer-value-must-not-leak",
        "endpoint": "https://private-user:private-pass@example.invalid/resource",
        "detail": "password=inline-value-must-not-leak",
    });
    store.record_diagnostic(sensitive).unwrap();
    let destination = temporary.path().join("exports").join("diagnostics.jsonl");
    let exported = store.export_diagnostics(&destination, true).unwrap();
    assert!(exported.sensitive_paths_redacted);
    assert_eq!(exported.record_count, 1);
    let bytes = fs::read(&destination).unwrap();
    assert_eq!(bytes.len() as u64, exported.byte_count);
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), exported.sha256);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("source_read_failed"));
    assert!(text.contains("correlation_id"));
    assert!(text.contains("redacted"));
    assert!(!text.contains("Private"));
    assert!(!text.contains("must-never-leak"));
    assert!(!text.contains("also-secret"));
    assert!(!text.contains("bearer-value-must-not-leak"));
    assert!(!text.contains("private-pass"));
    assert!(!text.contains("inline-value-must-not-leak"));
    assert_eq!(
        store.export_diagnostics(&destination, true).unwrap_err(),
        "diagnostic_export_target_exists"
    );

    let limited = DiagnosticRetentionPolicy {
        max_database_bytes: Some(1),
        ..DiagnosticRetentionPolicy::default()
    };
    assert!(
        store
            .run_diagnostic_retention(&limited)
            .unwrap()
            .capacity_exceeded
    );
}

#[test]
fn diagnostic_load_is_bounded_correlated_and_has_safe_japanese_presentations() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("load.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let mut known = event(
        "recovery",
        "logical-run",
        "published_journal_transition_failed",
    );
    known.attempt_id = Some("attempt-1".into());
    known.item_id = Some("item-1".into());
    known.sequence_no = Some(7);
    known.message_key = "workflow_recovery_required".into();
    known.payload = json!({
        "context": {"operation": "copy_publish_delete"},
        "cause_chain": [
            {"code": "journal_transition_db_failed"},
            {"code": "database_write_failed", "password": "never-load-this"}
        ]
    });
    let known_id = store.record_diagnostic(known).unwrap();
    let mut unknown = event("diagnostic", "other-run", "novel_internal_code");
    unknown.message_key = "diagnostic_unknown".into();
    let unknown_id = store.record_diagnostic(unknown).unwrap();

    let correlated = store.list_diagnostics(Some("attempt-1"), 10).unwrap();
    assert_eq!(correlated.len(), 1);
    let loaded = &correlated[0];
    assert_eq!(loaded.correlation_id, known_id);
    assert_eq!(loaded.attempt_id.as_deref(), Some("attempt-1"));
    assert_eq!(loaded.item_id.as_deref(), Some("item-1"));
    assert_eq!(loaded.sequence_no, Some(7));
    assert_eq!(loaded.message_key, "workflow_recovery_required");
    assert_eq!(loaded.payload["cause_chain"][1]["password"], "[REDACTED]");
    assert!(loaded.presentation.summary_ja.contains("復旧"));
    assert!(loaded
        .presentation
        .next_action_ja
        .contains(&loaded.correlation_id));

    assert_eq!(
        store.list_diagnostics(None, 1).unwrap().len(),
        1,
        "load must honor its hard page bound"
    );
    let unknown_rows = store.list_diagnostics(Some("other-run"), 10).unwrap();
    assert_eq!(unknown_rows.len(), 1);
    assert_eq!(unknown_rows[0].correlation_id, unknown_id);
    assert_eq!(
        unknown_rows[0].presentation.summary_ja,
        "詳細不明の診断が記録されました。"
    );
    assert!(!unknown_rows[0]
        .presentation
        .summary_ja
        .contains("novel_internal_code"));
    assert_eq!(
        store.list_diagnostics(None, 0).unwrap_err(),
        "diagnostic_page_limit_invalid"
    );
}

#[test]
fn byte_cap_never_evicts_fresh_or_protected_recovery_evidence() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("capacity.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let mut protected = event("recovery", "run", "workflow_recovery_required");
    protected.protected = true;
    let protected_id = store.record_diagnostic(protected).unwrap();
    let fresh_id = store
        .record_diagnostic(event("progress", "run", "fresh_progress"))
        .unwrap();
    let policy = DiagnosticRetentionPolicy {
        max_database_bytes: Some(1),
        ..DiagnosticRetentionPolicy::default()
    };
    let result = store.run_diagnostic_retention(&policy).unwrap();
    assert!(result.capacity_exceeded);
    let capacity_audit = store
        .list_diagnostics(Some(&result.retention_run_id), 10)
        .unwrap()
        .into_iter()
        .find(|event| event.code == "diagnostic_capacity_exceeded")
        .expect("hard-cap overage must be durable and operator-visible");
    assert_eq!(capacity_audit.class, "audit");
    assert_eq!(capacity_audit.severity, "warning");
    assert_eq!(
        capacity_audit.payload["capacity_eviction_deferred_by_safety_floor"],
        true
    );
    assert_eq!(capacity_audit.payload["max_database_bytes"], 1);
    let connection = Connection::open(&database).unwrap();
    for id in [protected_id, fresh_id] {
        let present: i64 = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM diagnostic_events WHERE id=?1)",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 1);
    }
}
