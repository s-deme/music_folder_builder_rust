use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard, OnceLock,
    },
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static MUTATION_TESTS: OnceLock<Mutex<()>> = OnceLock::new();

fn mutation_test_guard() -> MutexGuard<'static, ()> {
    MUTATION_TESTS
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(windows)]
use music_folder_core::ports::ApplyStore;
#[cfg(windows)]
use music_folder_infra::sqlite::SqliteScanStore;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "music-folder-cli-test-{}-{label}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_music-folder"))
        .args(arguments)
        .output()
        .expect("run music-folder CLI")
}

#[test]
fn doctor_cli_persists_filters_and_emits_one_json_document() {
    let temp = TestDirectory::new("doctor");
    let source = temp.join("source");
    fs::create_dir(&source).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3");
    fs::copy(&fixture, source.join("one.mp3")).unwrap();
    fs::copy(&fixture, source.join("two.mp3")).unwrap();
    fs::write(source.join("broken.mp3"), b"not audio").unwrap();
    let database = temp.join("doctor.db");
    let db = database.to_str().unwrap();
    let output = run(&[
        "--output",
        "json",
        "--events",
        "jsonl",
        "doctor",
        "scan",
        "--source",
        source.to_str().unwrap(),
        "--db",
        db,
    ]);
    assert_eq!(output.status.code(), Some(4));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "doctor.scan");
    assert_eq!(envelope["status"], "partial");
    let id = envelope["data"]["doctor_run_id"].as_str().unwrap();
    assert_eq!(envelope["correlation"]["attempt_id"], id);
    assert_eq!(envelope["data"]["files"], 3);
    let events: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert_eq!(events.last().unwrap()["event_type"], "terminal");
    let output = run(&[
        "--output",
        "json",
        "doctor",
        "issues",
        "--run-id",
        id,
        "--db",
        db,
        "--severity",
        "critical",
        "--code",
        "read_failed",
    ]);
    assert!(output.status.success());
    let issues = json_envelope(&output);
    assert_eq!(issues["data"]["issues"].as_array().unwrap().len(), 1);
    assert_eq!(issues["data"]["files"].as_array().unwrap().len(), 1);
    let output = run(&[
        "--output",
        "json",
        "doctor",
        "duplicates",
        "--run-id",
        id,
        "--db",
        db,
    ]);
    let groups = json_envelope(&output);
    assert_eq!(groups["data"]["issues"][0]["code"], "exact_duplicate");
    assert_eq!(groups["data"]["files"].as_array().unwrap().len(), 2);
    for command in ["show", "albums"] {
        let output = run(&[
            "--output", "json", "doctor", command, "--run-id", id, "--db", db,
        ]);
        assert!(output.status.success());
        json_envelope(&output);
    }
    let output = run(&[
        "--output", "json", "doctor", "issues", "--run-id", id, "--db", db, "--code", "nonsense",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json_envelope(&output)["command"], "doctor.issues");
    assert_eq!(
        fs::read(source.join("one.mp3")).unwrap(),
        fs::read(fixture).unwrap()
    );
}

fn run_with_env(arguments: &[&str], key: &str, value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_music-folder"))
        .args(arguments)
        .env(key, value)
        .output()
        .expect("run music-folder CLI with environment")
}

fn json_envelope(output: &Output) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout must be UTF-8");
    assert_eq!(stdout.lines().count(), 1, "JSON stdout must be one line");
    serde_json::from_str(&stdout).expect("stdout must be a JSON envelope")
}

fn assert_common_envelope(envelope: &Value, command: &str) {
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["schema_revision"]["major"], 1);
    assert_eq!(envelope["schema_revision"]["minor"], 1);
    assert_eq!(envelope["command"], command);
    assert!(envelope.get("result_type").is_some());
    assert!(envelope.get("correlation").is_some());
    assert!(envelope.get("counts").is_some());
    assert!(envelope["diagnostics"].is_array());
    assert!(envelope.get("result").is_some());
    assert!(envelope.get("status").is_some());
    assert!(envelope.get("data").is_some());
    assert!(envelope.get("error").is_some());
}

#[test]
fn json_history_list_emits_only_a_versioned_success_envelope() {
    let directory = TestDirectory::new("history-list");
    let database = directory.join("history.db");
    let output = run(&[
        "--output",
        "json",
        "history",
        "list",
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);

    assert_eq!(output.status.code(), Some(0));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "history.list");
    assert_eq!(envelope["status"], "success");
    assert!(envelope["data"].is_array());
    assert!(envelope["error"].is_null());
}

#[test]
fn empty_recovery_list_is_a_successful_json_envelope() {
    let directory = TestDirectory::new("recovery-list");
    let database = directory.join("recovery.db");
    let output = run(&[
        "--output",
        "json",
        "recovery",
        "list",
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);

    assert_eq!(output.status.code(), Some(0));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "recovery.list");
    assert_eq!(envelope["status"], "success");
    assert_eq!(envelope["data"], Value::Array(Vec::new()));
    assert!(envelope["error"].is_null());
}

#[test]
fn deterministic_cancellation_terminates_scan_with_stable_exit_and_durable_status() {
    let directory = TestDirectory::new("cancelled-exit");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3");
    fs::copy(fixture, source.join("candidate.mp3")).expect("copy cancellation fixture");
    let database = directory.join("cancelled.db");
    let output = run_with_env(
        &[
            "--output",
            "json",
            "--events",
            "jsonl",
            "scan",
            "--source",
            source.to_str().expect("UTF-8 source"),
            "--workers",
            "1",
            "--db",
            database.to_str().expect("UTF-8 database"),
        ],
        "MFB_FAULT_CANCEL_AFTER_MS",
        "0",
    );

    assert_eq!(
        output.status.code(),
        Some(9),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "scan");
    assert_eq!(envelope["status"], "cancelled");
    assert_eq!(envelope["error"]["code"], "scan_cancelled");
    let terminal = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["event_type"] == "terminal")
        .expect("terminal JSONL event");
    assert_eq!(terminal["payload"]["status"], "cancelled");
    assert_eq!(terminal["payload"]["exit_code"], 9);
    let connection = rusqlite::Connection::open(database).expect("open cancelled database");
    let status: String = connection
        .query_row("SELECT status FROM scan_runs", [], |row| row.get(0))
        .expect("cancelled scan row");
    assert_eq!(status, "cancelled");
}

#[test]
fn jsonl_event_stream_has_versioned_sequence_correlation_and_terminal_marker() {
    let directory = TestDirectory::new("event-stream");
    let database = directory.join("events.db");
    let output = run(&[
        "--output",
        "json",
        "--events",
        "jsonl",
        "history",
        "list",
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);

    assert_eq!(output.status.code(), Some(0));
    assert_common_envelope(&json_envelope(&output), "history.list");
    let stderr = String::from_utf8(output.stderr).expect("stderr must be UTF-8");
    let events = stderr
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<Value>(line).expect("JSONL event"))
        .collect::<Vec<_>>();
    assert!(!events.is_empty(), "terminal event must be emitted");
    let correlation = events[0]["correlation_id"]
        .as_str()
        .expect("correlation ID");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["schema_version"], 1);
        assert_eq!(event["sequence"], (index + 1) as u64);
        assert_eq!(event["correlation_id"], correlation);
        assert_eq!(event["command"], "history.list");
    }
    let terminal = events.last().expect("terminal event");
    assert_eq!(terminal["event_type"], "terminal");
    assert_eq!(terminal["payload"]["terminal"], true);
    assert_eq!(terminal["payload"]["exit_code"], 0);
}

#[test]
fn jsonl_usage_failure_has_one_versioned_terminal_event() {
    let output = run(&[
        "--output",
        "json",
        "--events",
        "jsonl",
        "plan",
        "revise",
        "--plan-run-id",
        "plan-1",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "plan.revise");
    let events = String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("stderr must be JSONL"))
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["schema_version"], 1);
    assert_eq!(events[0]["event_type"], "terminal");
    assert_eq!(events[0]["sequence"], 1);
    assert_eq!(events[0]["command"], "plan.revise");
    assert_eq!(events[0]["payload"]["exit_code"], 2);
}

#[test]
fn apply_execute_without_exact_plan_confirmation_is_blocked_before_database_open() {
    let output = run(&[
        "--output",
        "json",
        "--yes",
        "apply",
        "--plan-run-id",
        "plan-1",
        "--execute",
        "--db",
        "missing-parent/unused.db",
    ]);
    assert_eq!(output.status.code(), Some(3));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "apply");
    assert_eq!(envelope["error"]["code"], "apply_confirmation_required");
}

#[test]
fn non_interactive_apply_requires_yes_before_database_open() {
    let output = run(&[
        "--output",
        "json",
        "apply",
        "--plan-run-id",
        "plan-1",
        "--execute",
        "--confirm",
        "plan-1",
        "--db",
        "missing-parent/unused.db",
    ]);

    assert_eq!(output.status.code(), Some(3));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "apply");
    assert_eq!(
        envelope["error"]["code"],
        "non_interactive_confirmation_required"
    );
}

#[cfg(windows)]
#[test]
fn second_cli_process_reports_stable_lease_busy_exit() {
    let _guard = mutation_test_guard();
    let directory = TestDirectory::new("lease-busy");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let database = directory.join("workflow.db");
    let target = directory.join("target");
    let scan = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 source"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    let scan_id = json_envelope(&scan)["result"]["scan_run_id"]
        .as_str()
        .expect("scan ID")
        .to_owned();
    let plan = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_id,
        "--target",
        target.to_str().expect("UTF-8 target"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    let plan_id = json_envelope(&plan)["result"]["plan_run_id"]
        .as_str()
        .expect("plan ID")
        .to_owned();
    let store = SqliteScanStore::open(&database).expect("open store");
    let lease = store
        .acquire_apply_lease(&plan_id, "holding-process")
        .expect("hold lease");

    let contender = run(&[
        "--output",
        "json",
        "--yes",
        "apply",
        "--plan-run-id",
        &plan_id,
        "--execute",
        "--confirm",
        &plan_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(contender.status.code(), Some(6));
    let envelope = json_envelope(&contender);
    assert_eq!(envelope["status"], "lease_busy");
    assert!(matches!(
        envelope["error"]["code"].as_str(),
        Some("mutation_scope_busy" | "mutation_lease_busy")
    ));
    store.release_apply_lease(&lease).expect("release lease");
}

#[test]
fn invalid_plan_toml_is_usage_error_two_in_json_mode() {
    let directory = TestDirectory::new("invalid-config");
    let config = directory.join("invalid.toml");
    fs::write(&config, "[naming]\nunknown_option = true\n").expect("write config");
    let database = directory.join("unused.db");
    let target = directory.join("target");
    let output = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        "scan-1",
        "--target",
        target.to_str().expect("UTF-8 temp path"),
        "--config",
        config.to_str().expect("UTF-8 temp path"),
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);

    assert_eq!(output.status.code(), Some(2));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "plan");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "invalid_config");
}

#[test]
fn oversized_config_and_out_of_range_typed_inputs_exit_two() {
    let directory = TestDirectory::new("bounded-inputs");
    let config = directory.join("oversized.toml");
    fs::write(&config, vec![b' '; 65_537]).expect("write oversized config");
    let target = directory.join("target");
    let oversized = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        "scan-1",
        "--target",
        target.to_str().expect("UTF-8 target"),
        "--config",
        config.to_str().expect("UTF-8 config"),
    ]);
    assert_eq!(oversized.status.code(), Some(2));
    assert_eq!(
        json_envelope(&oversized)["error"]["code"],
        "config_too_large"
    );

    for arguments in [
        vec![
            "--output",
            "json",
            "scan",
            "--source",
            ".",
            "--workers",
            "257",
        ],
        vec![
            "--output", "json", "history", "list", "--status", "invented",
        ],
        vec!["--output", "json", "history", "list", "--limit", "201"],
    ] {
        let output = run(&arguments);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(json_envelope(&output)["error"]["code"], "usage_error");
    }
}

#[test]
fn explicit_plan_flags_override_naming_config_values() {
    let directory = TestDirectory::new("config-precedence");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create empty source");
    let database = directory.join("workflow.db");
    let scan_output = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 temp path"),
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(scan_output.status.code(), Some(0));
    let scan_envelope = json_envelope(&scan_output);
    let scan_run_id = scan_envelope["data"]["scan_run_id"]
        .as_str()
        .expect("scan run ID")
        .to_owned();

    let config = directory.join("plan.toml");
    fs::write(
        &config,
        "[naming]\nartist_dir_template = 'from-config'\nduplicate_strategy = 'template'\nallow_long_paths = false\n",
    )
    .expect("write config");
    let target = directory.join("target");
    let output = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_run_id,
        "--target",
        target.to_str().expect("UTF-8 temp path"),
        "--config",
        config.to_str().expect("UTF-8 temp path"),
        "--artist-dir-template",
        "from-cli",
        "--duplicate-strategy",
        "sequence",
        "--allow-long-paths",
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);

    assert_eq!(output.status.code(), Some(0));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "plan");
    assert_eq!(
        envelope["data"]["naming"]["artist_dir_template"],
        "from-cli"
    );
    assert_eq!(envelope["data"]["naming"]["duplicate_strategy"], "sequence");
    assert!(envelope["data"]["naming"]["allow_long_paths"]
        .as_bool()
        .expect("allow_long_paths boolean"));
}

#[test]
fn plan_revision_apply_verify_and_confirmed_rollback_complete_through_the_cli() {
    let _guard = mutation_test_guard();
    let directory = TestDirectory::new("full-cli-workflow");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let original = source.join("track.mp3");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3"),
        &original,
    )
    .expect("copy audio fixture");
    let database = directory.join("workflow.db");
    let target_root = directory.join("target");

    let scan = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 source"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(scan.status.code(), Some(0));
    let scan_id = json_envelope(&scan)["result"]["scan_run_id"]
        .as_str()
        .expect("scan ID")
        .to_owned();

    let plan = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_id,
        "--target",
        target_root.to_str().expect("UTF-8 target"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(plan.status.code(), Some(0));
    let plan_id = json_envelope(&plan)["result"]["plan_run_id"]
        .as_str()
        .expect("plan ID")
        .to_owned();
    let connection = rusqlite::Connection::open(&database).expect("open workflow database");
    let item_id: String = connection
        .query_row(
            "SELECT id FROM plan_items WHERE plan_id=?1 ORDER BY ordinal LIMIT 1",
            [&plan_id],
            |row| row.get(0),
        )
        .expect("plan item ID");
    drop(connection);
    let revised_target = target_root.join("manual").join("track.mp3");
    let revision = run(&[
        "--output",
        "json",
        "plan",
        "revise",
        "--plan-run-id",
        &plan_id,
        "--manual",
        &item_id,
        revised_target.to_str().expect("UTF-8 manual target"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(revision.status.code(), Some(0));
    let revised_plan_id = json_envelope(&revision)["result"]["plan_run_id"]
        .as_str()
        .expect("revised plan ID")
        .to_owned();

    let apply = run(&[
        "--output",
        "json",
        "--yes",
        "apply",
        "--plan-run-id",
        &revised_plan_id,
        "--execute",
        "--confirm",
        &revised_plan_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    let apply_logs = operation_log_errors(&database);
    assert_eq!(
        apply.status.code(),
        Some(0),
        "stdout={} stderr={} logs={apply_logs:?}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    let execution_id = json_envelope(&apply)["result"]["execution_run_id"]
        .as_str()
        .expect("execution ID")
        .to_owned();
    assert!(!original.exists());
    assert!(revised_target.exists());

    let verify_apply = run(&[
        "--output",
        "json",
        "verify",
        "--execution-run-id",
        &execution_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(verify_apply.status.code(), Some(0));

    let rollback = run(&[
        "--output",
        "json",
        "--yes",
        "rollback",
        "--execution-run-id",
        &execution_id,
        "--execute",
        "--confirm",
        &execution_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(rollback.status.code(), Some(0));
    let rollback_id = json_envelope(&rollback)["result"]["rollback_run_id"]
        .as_str()
        .expect("rollback ID")
        .to_owned();
    assert!(original.exists());
    assert!(!revised_target.exists());

    let verify_rollback = run(&[
        "--output",
        "json",
        "verify",
        "--subject",
        "rollback",
        "--subject-id",
        &rollback_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(verify_rollback.status.code(), Some(0));
    let verified = json_envelope(&verify_rollback);
    assert_eq!(verified["result"]["subject_kind"], "rollback");
    assert_eq!(verified["result"]["subject_id"], rollback_id);
}

#[test]
fn opaque_conflict_candidate_creates_a_lossless_child_plan() {
    let directory = TestDirectory::new("opaque-candidate");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3");
    fs::copy(&fixture, source.join("one.mp3")).expect("copy first music fixture");
    fs::copy(&fixture, source.join("two.mp3")).expect("copy second music fixture");
    fs::write(source.join("cover.jpg"), b"image fixture").expect("write image fixture");
    let database = directory.join("workflow.db");
    let target_root = directory.join("target");

    let scan = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 source"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(scan.status.code(), Some(0));
    let scan_id = json_envelope(&scan)["result"]["scan_run_id"]
        .as_str()
        .expect("scan ID")
        .to_owned();
    let plan = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_id,
        "--target",
        target_root.to_str().expect("UTF-8 target"),
        "--artist-dir-template",
        "{source_stem}",
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(
        plan.status.code(),
        Some(3),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan_id = json_envelope(&plan)["result"]["plan_run_id"]
        .as_str()
        .expect("plan ID")
        .to_owned();
    let connection = rusqlite::Connection::open(&database).expect("open candidate database");
    let (image_item_id, conflict_group_id): (String, String) = connection
        .query_row(
            "SELECT id,conflict_group_id FROM plan_items
              WHERE plan_id=?1 AND source_kind='image'",
            [&plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("ambiguous image item");
    let music_item_id: String = connection
        .query_row(
            "SELECT id FROM plan_items
              WHERE plan_id=?1 AND source_kind='music' ORDER BY ordinal LIMIT 1",
            [&plan_id],
            |row| row.get(0),
        )
        .expect("music item");
    let (candidate_encoding, candidate_raw): (String, Vec<u8>) = connection
        .query_row(
            "SELECT target_path_encoding,target_path_blob
               FROM plan_conflict_candidates
              WHERE conflict_group_id=?1 AND ordinal=1",
            [&conflict_group_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("candidate identity");
    drop(connection);

    let wrong_binding = format!("{music_item_id}:{conflict_group_id}:1");
    let rejected = run(&[
        "--output",
        "json",
        "plan",
        "revise",
        "--plan-run-id",
        &plan_id,
        "--candidate",
        &wrong_binding,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(rejected.status.code(), Some(3));
    assert_eq!(
        json_envelope(&rejected)["error"]["code"],
        "plan_conflict_candidate_not_found"
    );

    let selection = format!("{image_item_id}:{conflict_group_id}:1");
    let revision = run(&[
        "--output",
        "json",
        "plan",
        "revise",
        "--plan-run-id",
        &plan_id,
        "--candidate",
        &selection,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(revision.status.code(), Some(0));
    let revised = json_envelope(&revision);
    assert_eq!(revised["result"]["candidate_changes"], 1);
    let child_plan_id = revised["result"]["plan_run_id"]
        .as_str()
        .expect("child Plan ID");
    let (target_encoding, target_raw): (String, Vec<u8>) = rusqlite::Connection::open(&database)
        .expect("reopen candidate database")
        .query_row(
            "SELECT target_path_encoding,target_path_blob FROM plan_items
                  WHERE plan_id=?1 AND source_kind='image'",
            [child_plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("revised image target");
    let candidate_directory =
        music_folder_infra::path_codec::decode_path(&candidate_encoding, &candidate_raw).unwrap();
    let revised_target =
        music_folder_infra::path_codec::decode_path(&target_encoding, &target_raw).unwrap();
    assert_eq!(revised_target, candidate_directory.join("cover.jpg"));
}

#[test]
fn apply_reports_stable_partial_exit_when_one_source_changes_after_plan() {
    let _guard = mutation_test_guard();
    let directory = TestDirectory::new("partial-exit");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3");
    let changed = source.join("changed.mp3");
    let movable = source.join("movable.mp3");
    fs::copy(&fixture, &changed).expect("copy changed fixture");
    fs::copy(&fixture, &movable).expect("copy movable fixture");
    let database = directory.join("workflow.db");
    let target = directory.join("target");

    let scan = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 source"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(scan.status.code(), Some(0));
    let scan_id = json_envelope(&scan)["result"]["scan_run_id"]
        .as_str()
        .expect("scan ID")
        .to_owned();
    let plan = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_id,
        "--target",
        target.to_str().expect("UTF-8 target"),
        "--use-source-filename",
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(plan.status.code(), Some(0));
    let plan_id = json_envelope(&plan)["result"]["plan_run_id"]
        .as_str()
        .expect("plan ID")
        .to_owned();
    fs::remove_file(&changed).expect("replace planned source with missing state");

    let apply = run(&[
        "--output",
        "json",
        "--yes",
        "apply",
        "--plan-run-id",
        &plan_id,
        "--execute",
        "--confirm",
        &plan_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    let apply_logs = operation_log_errors(&database);
    assert_eq!(
        apply.status.code(),
        Some(4),
        "stdout={} stderr={} logs={apply_logs:?}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    let envelope = json_envelope(&apply);
    assert_common_envelope(&envelope, "apply");
    assert_eq!(envelope["status"], "partial");
    assert_eq!(envelope["result"]["success"], 1);
    assert_eq!(envelope["result"]["failed"], 1);
    assert_eq!(envelope["diagnostics"][0]["code"], "command_partial");
    assert!(!movable.exists(), "unchanged source should be moved");
}

#[test]
fn recovery_list_reports_stable_recovery_required_exit_for_abandoned_journal() {
    let _guard = mutation_test_guard();
    let directory = TestDirectory::new("recovery-required-exit");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3");
    fs::copy(&fixture, source.join("recoverable.mp3")).expect("copy fixture");
    let database = directory.join("workflow.db");
    let target = directory.join("target");

    let scan = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 source"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(scan.status.code(), Some(0));
    let scan_id = json_envelope(&scan)["result"]["scan_run_id"]
        .as_str()
        .expect("scan ID")
        .to_owned();
    let plan = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_id,
        "--target",
        target.to_str().expect("UTF-8 target"),
        "--use-source-filename",
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(plan.status.code(), Some(0));
    let plan_id = json_envelope(&plan)["result"]["plan_run_id"]
        .as_str()
        .expect("plan ID")
        .to_owned();
    let apply = run(&[
        "--output",
        "json",
        "--yes",
        "apply",
        "--plan-run-id",
        &plan_id,
        "--execute",
        "--confirm",
        &plan_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(apply.status.code(), Some(0));
    let execution_id = json_envelope(&apply)["result"]["execution_run_id"]
        .as_str()
        .expect("execution ID")
        .to_owned();

    let connection = rusqlite::Connection::open(&database).expect("open workflow database");
    let operation_id: String = connection
        .query_row(
            "SELECT id FROM operation_journal WHERE attempt_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .expect("completed journal");
    assert_eq!(
        connection
            .execute(
                "UPDATE operation_journal SET state='published' WHERE id=?1",
                [&operation_id],
            )
            .expect("simulate process death after publish"),
        1
    );
    drop(connection);

    let recovery = run(&[
        "--output",
        "json",
        "recovery",
        "list",
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(recovery.status.code(), Some(7));
    let envelope = json_envelope(&recovery);
    assert_common_envelope(&envelope, "recovery.list");
    assert_eq!(envelope["status"], "recovery_required");
    assert_eq!(envelope["result"][0]["operation_id"], operation_id);
    assert_eq!(envelope["diagnostics"][0]["code"], "recovery_required");

    let dry_run = run(&[
        "--output",
        "json",
        "recovery",
        "run",
        "--operation-id",
        &operation_id,
        "--action",
        "resume",
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(dry_run.status.code(), Some(7));
    let dry_run = json_envelope(&dry_run);
    assert_common_envelope(&dry_run, "recovery.run");
    assert_eq!(dry_run["status"], "recovery_required");
    assert_eq!(dry_run["result"]["operation_id"], operation_id);
    assert_eq!(dry_run["result"]["mode"], "dry_run");

    let executed = run(&[
        "--output",
        "json",
        "--yes",
        "recovery",
        "run",
        "--operation-id",
        &operation_id,
        "--action",
        "resume",
        "--execute",
        "--confirm",
        &operation_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(
        executed.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&executed.stdout),
        String::from_utf8_lossy(&executed.stderr)
    );
    let executed = json_envelope(&executed);
    assert_common_envelope(&executed, "recovery.run");
    assert_eq!(executed["result"]["operation_id"], operation_id);
    assert_eq!(executed["result"]["mode"], "recovery");
    let recovery_run_id = executed["result"]["recovery_run_id"]
        .as_str()
        .expect("recovery run ID");

    let verify = run(&[
        "--output",
        "json",
        "verify",
        "--subject",
        "recovery",
        "--subject-id",
        recovery_run_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(
        verify.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    let verify = json_envelope(&verify);
    assert_common_envelope(&verify, "verify");
    assert_eq!(verify["result"]["subject_kind"], "recovery");
    assert_eq!(verify["result"]["subject_id"], recovery_run_id);
}

#[test]
fn verify_reports_stable_mismatch_exit_after_target_tamper() {
    let _guard = mutation_test_guard();
    let directory = TestDirectory::new("verify-mismatch-exit");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create source");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../infra/tests/fixtures/mp3/japanese.mp3");
    fs::copy(&fixture, source.join("tamper.mp3")).expect("copy fixture");
    let database = directory.join("workflow.db");
    let target_root = directory.join("target");

    let scan = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 source"),
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(scan.status.code(), Some(0));
    let scan_id = json_envelope(&scan)["result"]["scan_run_id"]
        .as_str()
        .expect("scan ID")
        .to_owned();
    let plan = run(&[
        "--output",
        "json",
        "plan",
        "--scan-run-id",
        &scan_id,
        "--target",
        target_root.to_str().expect("UTF-8 target"),
        "--use-source-filename",
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(plan.status.code(), Some(0));
    let plan_id = json_envelope(&plan)["result"]["plan_run_id"]
        .as_str()
        .expect("plan ID")
        .to_owned();
    let apply = run(&[
        "--output",
        "json",
        "--yes",
        "apply",
        "--plan-run-id",
        &plan_id,
        "--execute",
        "--confirm",
        &plan_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(apply.status.code(), Some(0));
    let execution_id = json_envelope(&apply)["result"]["execution_run_id"]
        .as_str()
        .expect("execution ID")
        .to_owned();
    let connection = rusqlite::Connection::open(&database).expect("open workflow database");
    let target: String = connection
        .query_row(
            "SELECT target_path FROM operation_logs WHERE execution_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .expect("operation target");
    drop(connection);
    fs::write(&target, b"tampered target bytes").expect("tamper target");

    let verify = run(&[
        "--output",
        "json",
        "verify",
        "--execution-run-id",
        &execution_id,
        "--db",
        database.to_str().expect("UTF-8 database"),
    ]);
    assert_eq!(verify.status.code(), Some(8));
    let envelope = json_envelope(&verify);
    assert_common_envelope(&envelope, "verify");
    assert_eq!(envelope["status"], "verify_mismatch");
    assert_eq!(envelope["result"]["subject_kind"], "execution");
    assert_eq!(envelope["result"]["subject_id"], execution_id);
    assert_eq!(envelope["result"]["failed"], 1);
    assert_eq!(envelope["diagnostics"][0]["code"], "verify_mismatch");
    assert_eq!(fs::read(target).unwrap(), b"tampered target bytes");
}

fn operation_log_errors(database: &Path) -> Vec<(String, String, Option<String>)> {
    let connection = rusqlite::Connection::open(database).expect("open operation log database");
    let mut statement = connection
        .prepare("SELECT action,result,error FROM operation_logs ORDER BY sequence_no,id")
        .expect("prepare operation log query");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query operation logs")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect operation logs")
}

#[test]
fn rollback_execute_without_exact_confirmation_is_blocked_before_database_open() {
    let output = run(&[
        "--output",
        "json",
        "rollback",
        "--execution-run-id",
        "execution-1",
        "--execute",
        "--db",
        "missing-parent/unused.db",
    ]);

    assert_eq!(output.status.code(), Some(3));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "rollback");
    assert_eq!(envelope["status"], "blocked");
    assert_eq!(envelope["error"]["code"], "rollback_confirmation_required");
}

#[test]
fn history_delete_without_exact_confirmation_is_blocked_before_database_open() {
    let output = run(&[
        "--output",
        "json",
        "history",
        "delete",
        "--kind",
        "scan",
        "--run-id",
        "scan-1",
        "--db",
        "missing-parent/unused.db",
    ]);

    assert_eq!(output.status.code(), Some(3));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "history.delete");
    assert_eq!(envelope["status"], "blocked");
    assert_eq!(
        envelope["error"]["code"],
        "history_delete_confirmation_required"
    );
}

#[test]
fn history_archive_is_verified_before_confirmed_purge() {
    let directory = TestDirectory::new("history-delete");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create empty source");
    let database = directory.join("workflow.db");
    let scan_output = run(&[
        "--output",
        "json",
        "scan",
        "--source",
        source.to_str().expect("UTF-8 temp path"),
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(scan_output.status.code(), Some(0));
    let scan_envelope = json_envelope(&scan_output);
    let scan_run_id = scan_envelope["data"]["scan_run_id"]
        .as_str()
        .expect("scan run ID")
        .to_owned();

    let preview_output = run(&[
        "--output",
        "json",
        "history",
        "cleanup-preview",
        "--kind",
        "scan",
        "--run-id",
        &scan_run_id,
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(preview_output.status.code(), Some(0));
    let preview = json_envelope(&preview_output);
    assert_common_envelope(&preview, "history.cleanup-preview");
    assert!(!preview["data"]["blocked"]
        .as_bool()
        .expect("blocked boolean"));

    let premature_delete = run(&[
        "--output",
        "json",
        "--yes",
        "history",
        "delete",
        "--kind",
        "scan",
        "--run-id",
        &scan_run_id,
        "--confirm",
        &scan_run_id,
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(premature_delete.status.code(), Some(3));
    let premature_delete = json_envelope(&premature_delete);
    assert_common_envelope(&premature_delete, "history.delete");
    assert_eq!(
        premature_delete["error"]["code"],
        "verified_history_archive_required"
    );

    let archive_directory = directory.join("archives");
    let archive_output = run(&[
        "--output",
        "json",
        "--yes",
        "history",
        "archive",
        "--kind",
        "scan",
        "--run-id",
        &scan_run_id,
        "--archive-dir",
        archive_directory.to_str().expect("UTF-8 temp path"),
        "--confirm",
        &scan_run_id,
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(archive_output.status.code(), Some(0));
    let archived = json_envelope(&archive_output);
    assert_common_envelope(&archived, "history.archive");
    assert!(archived["data"]["verified"]
        .as_bool()
        .expect("verified boolean"));
    assert!(std::path::Path::new(
        archived["data"]["archive_path"]["display"]
            .as_str()
            .expect("archive path")
    )
    .is_file());

    let delete_output = run(&[
        "--output",
        "json",
        "--yes",
        "history",
        "delete",
        "--kind",
        "scan",
        "--run-id",
        &scan_run_id,
        "--confirm",
        &scan_run_id,
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(delete_output.status.code(), Some(0));
    let deleted = json_envelope(&delete_output);
    assert_common_envelope(&deleted, "history.delete");
    assert!(deleted["data"]["deleted"]
        .as_bool()
        .expect("deleted boolean"));
}

#[test]
fn semantic_usage_error_uses_exit_two_and_json_envelope() {
    let output = run(&[
        "--output",
        "json",
        "plan",
        "revise",
        "--plan-run-id",
        "plan-1",
    ]);

    assert_eq!(output.status.code(), Some(2));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "plan.revise");
    assert_eq!(envelope["error"]["code"], "manual_target_change_required");
}

#[test]
fn recovery_execute_without_exact_confirmation_is_blocked_before_database_open() {
    let output = run(&[
        "--output",
        "json",
        "recovery",
        "run",
        "--operation-id",
        "operation-1",
        "--execute",
        "--db",
        "missing-parent/unused.db",
    ]);

    assert_eq!(output.status.code(), Some(3));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "recovery.run");
    assert_eq!(envelope["error"]["code"], "recovery_confirmation_required");
}

#[test]
fn diagnostics_retention_is_dry_run_by_default_and_export_is_redacted() {
    let directory = TestDirectory::new("diagnostics");
    let database = directory.join("workflow.db");
    let preview_output = run(&[
        "--output",
        "json",
        "diagnostics",
        "retention",
        "--db",
        database.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(preview_output.status.code(), Some(0));
    let preview = json_envelope(&preview_output);
    assert_common_envelope(&preview, "diagnostics.retention");
    assert!(preview["data"]["dry_run"].as_bool().expect("dry-run flag"));

    let export_path = directory.join("diagnostics.jsonl");
    let export_output = run(&[
        "--output",
        "json",
        "diagnostics",
        "export",
        "--db",
        database.to_str().expect("UTF-8 temp path"),
        "--destination",
        export_path.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(export_output.status.code(), Some(0));
    let exported = json_envelope(&export_output);
    assert_common_envelope(&exported, "diagnostics.export");
    assert!(exported["data"]["sensitive_paths_redacted"]
        .as_bool()
        .expect("redaction flag"));
    assert_eq!(exported["data"]["export_path"]["role"], "diagnostic_export");
    assert!(exported["data"]["export_path"]["raw_base64"].is_string());
    assert!(export_path.is_file());
}

#[test]
fn benchmark_reports_three_iteration_medians_plan_apply_and_peak_rss() {
    let directory = TestDirectory::new("benchmark-v2");
    let source = directory.join("source");
    fs::create_dir_all(&source).expect("create empty benchmark source");
    let output = run(&[
        "--output",
        "json",
        "benchmark",
        "--source",
        source.to_str().expect("UTF-8 temp path"),
        "--db",
        directory
            .join("benchmark.db")
            .to_str()
            .expect("UTF-8 temp path"),
    ]);
    assert_eq!(output.status.code(), Some(0));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "benchmark");
    assert_eq!(envelope["data"]["benchmark_schema_version"], 2);
    assert_eq!(envelope["data"]["iteration_count"], 3);
    assert_eq!(
        envelope["data"]["iterations"]
            .as_array()
            .expect("iterations")
            .len(),
        3
    );
    assert_eq!(envelope["data"]["cold"]["statistic"], "median");
    assert_eq!(envelope["data"]["warm"]["statistic"], "median");
    assert_eq!(envelope["data"]["plan"]["statistic"], "median");
    assert_eq!(envelope["data"]["apply_dry_run"]["statistic"], "median");
    assert_eq!(envelope["data"]["rss_sampling_interval_ms"], 5);
}

#[test]
fn completions_are_available_without_optional_dependencies() {
    let output = run(&["--output", "json", "completions", "powershell"]);

    assert_eq!(output.status.code(), Some(0));
    let envelope = json_envelope(&output);
    assert_common_envelope(&envelope, "completions");
    assert_eq!(envelope["status"], "success");
    assert!(envelope["data"]["script"]
        .as_str()
        .expect("completion script")
        .contains("Register-ArgumentCompleter"));
}
