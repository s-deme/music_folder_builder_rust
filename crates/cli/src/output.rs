use crate::args::OutputFormat;
use serde_json::{json, Value};

pub const EXIT_USAGE: i32 = 2;
pub const EXIT_BLOCKED: i32 = 3;
pub const EXIT_PARTIAL: i32 = 4;
pub const EXIT_INTERNAL: i32 = 5;
pub const EXIT_LEASE_BUSY: i32 = 6;
pub const EXIT_RECOVERY_REQUIRED: i32 = 7;
pub const EXIT_VERIFY_MISMATCH: i32 = 8;
pub const EXIT_CANCELLED: i32 = 9;

#[derive(Debug)]
pub struct CommandResult {
    pub status: &'static str,
    pub data: Value,
    pub human: String,
    pub exit_code: i32,
}

impl CommandResult {
    pub fn success(data: Value, human: impl Into<String>) -> Self {
        Self {
            status: "success",
            data,
            human: human.into(),
            exit_code: 0,
        }
    }

    pub fn blocked(data: Value, human: impl Into<String>) -> Self {
        Self {
            status: "blocked",
            data,
            human: human.into(),
            exit_code: EXIT_BLOCKED,
        }
    }

    pub fn partial(data: Value, human: impl Into<String>) -> Self {
        Self {
            status: "partial",
            data,
            human: human.into(),
            exit_code: EXIT_PARTIAL,
        }
    }

    pub fn recovery_required(data: Value, human: impl Into<String>) -> Self {
        Self {
            status: "recovery_required",
            data,
            human: human.into(),
            exit_code: EXIT_RECOVERY_REQUIRED,
        }
    }

    pub fn verify_mismatch(data: Value, human: impl Into<String>) -> Self {
        Self {
            status: "verify_mismatch",
            data,
            human: human.into(),
            exit_code: EXIT_VERIFY_MISMATCH,
        }
    }
}

#[derive(Debug)]
pub struct CliFailure {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
}

impl CliFailure {
    pub fn usage(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code: EXIT_USAGE,
        }
    }

    pub fn blocked(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code: EXIT_BLOCKED,
        }
    }

    pub fn internal(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code: EXIT_INTERNAL,
        }
    }

    pub fn recovery_required(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            exit_code: EXIT_RECOVERY_REQUIRED,
        }
    }

    pub fn from_workflow(error: music_folder_core::WorkflowError) -> Self {
        Self::from_code(error.code())
    }

    pub fn from_code(code: &str) -> Self {
        let exit_code = if is_usage(code) {
            EXIT_USAGE
        } else if is_cancelled(code) {
            EXIT_CANCELLED
        } else if is_lease_busy(code) {
            EXIT_LEASE_BUSY
        } else if is_recovery_required(code) {
            EXIT_RECOVERY_REQUIRED
        } else if is_verify_mismatch(code) {
            EXIT_VERIFY_MISMATCH
        } else if is_blocking(code) {
            EXIT_BLOCKED
        } else {
            EXIT_INTERNAL
        };
        Self {
            code: stable_code(code),
            message: code.to_owned(),
            exit_code,
        }
    }
}

fn is_cancelled(code: &str) -> bool {
    matches!(
        code.split(':').next(),
        Some("cancelled" | "scan_cancelled" | "plan_cancelled")
    )
}

fn is_lease_busy(code: &str) -> bool {
    matches!(
        code.split(':').next(),
        Some("mutation_lease_busy" | "mutation_scope_busy" | "database_busy")
    )
}

fn is_verify_mismatch(code: &str) -> bool {
    code.starts_with("verify_")
        || matches!(
            code.split(':').next(),
            Some("target_changed_since_apply" | "target_missing" | "source_already_exists")
        )
}

fn is_usage(code: &str) -> bool {
    code.starts_with("invalid_naming_rules")
        || matches!(
            code.split(':').next().unwrap_or("internal_error"),
            "invalid_duplicate_strategy" | "invalid_run_kind" | "manual_target_change_required"
        )
}

fn stable_code(code: &str) -> String {
    code.split(':')
        .next()
        .unwrap_or("internal_error")
        .to_owned()
}

fn is_recovery_required(code: &str) -> bool {
    code.contains("recovery_required")
        || code.contains("operation_journal_incomplete")
        || code.contains("unfinished_operation")
        || code.starts_with("published_")
        || code.starts_with("source_delete_failed")
        || code.starts_with("rollback_target_delete_failed")
}

fn is_blocking(code: &str) -> bool {
    const EXACT: &[&str] = &[
        "already_applied_for_plan",
        "companion_target_ambiguous",
        "expected_move_state_not_found",
        "manual_intervention_required",
        "legacy_plan_non_executable",
        "legacy_plan_non_revisable",
        "mutation_lease_busy",
        "mutation_scope_busy",
        "plan_conflict_not_found",
        "plan_conflict_candidate_not_authorizable",
        "plan_conflict_candidate_not_found",
        "plan_not_completed",
        "plan_not_revisable",
        "plan_item_not_found",
        "plan_snapshot_mismatch",
        "plan_snapshot_missing",
        "recovery_content_mismatch",
        "recovery_content_missing",
        "recovery_discard_after_publish_forbidden",
        "recovery_item_not_found",
        "recovery_size_mismatch",
        "recovery_temporary_missing",
        "running_run_cannot_be_deleted",
        "dependent_running_run_cannot_be_deleted",
        "source_already_exists",
        "source_changed_since_scan",
        "source_missing",
        "source_not_regular_file",
        "target_already_exists",
        "target_changed_since_apply",
        "target_conflict",
        "target_equals_root",
        "target_missing",
        "target_outside_root",
        "target_parent_missing",
        "run_not_found",
        "scan_root_not_found",
        "verified_history_archive_required",
        "archive_target_exists",
    ];
    let stable = code.split(':').next().unwrap_or("internal_error");
    EXACT.contains(&stable)
        || code.starts_with("invalid_target")
        || code.starts_with("preflight_")
        || code.starts_with("history_archive_blocked")
        || code.starts_with("history_cleanup_blocked")
        || code.starts_with("history_purge_blocked")
        || code.starts_with("plan_source_")
        || code.starts_with("plan_item_target_unsafe")
        || code.starts_with("plan_conflict_candidate_target_unsafe")
        || code.starts_with("scan_snapshot_")
        || code.starts_with("unsafe_")
        || code.starts_with("source_changed_")
        || code.contains("snapshot_mismatch")
        || code.contains("reparse_point")
}

pub fn emit_result(format: OutputFormat, command: &str, result: CommandResult) -> i32 {
    match format {
        OutputFormat::Human => println!("{}", result.human),
        OutputFormat::Json => {
            let (correlation, counts) = contract_context(&result.data);
            let diagnostics =
                result_diagnostics(command, result.status, &result.data, &correlation);
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "schema_version": 1,
                    "schema_revision": { "major": 1, "minor": 1 },
                    "command": command,
                    "status": result.status,
                    "result_type": command.replace('.', "_"),
                    "correlation": correlation,
                    "counts": counts,
                    "diagnostics": diagnostics,
                    "result": result.data,
                    "data": result.data,
                    "error": Value::Null,
                }))
                .expect("JSON envelope serialization cannot fail")
            )
        }
    }
    result.exit_code
}

fn result_diagnostics(command: &str, status: &str, data: &Value, correlation: &Value) -> Value {
    if status == "success" {
        return Value::Array(Vec::new());
    }
    let code = match status {
        "partial" => "command_partial",
        "blocked" => "command_blocked",
        "recovery_required" => "recovery_required",
        "verify_mismatch" => "verify_mismatch",
        _ => "command_not_successful",
    };
    let correlation_id = correlation
        .get("attempt_id")
        .and_then(Value::as_str)
        .or_else(|| correlation.get("subject_id").and_then(Value::as_str));
    json!([{
        "code": code,
        "severity": if status == "partial" || status == "verify_mismatch" { "error" } else { "warning" },
        "phase": command,
        "message_key": code,
        "correlation_id": correlation_id,
        "context": {
            "status": status,
            "counts": contract_context(data).1,
        },
    }])
}

pub fn emit_failure(format: OutputFormat, command: &str, failure: CliFailure) -> i32 {
    match format {
        OutputFormat::Human => eprintln!("error[{}]: {}", failure.code, failure.message),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "schema_revision": { "major": 1, "minor": 1 },
                "command": command,
                "status": match failure.exit_code {
                    EXIT_USAGE => "error",
                    EXIT_BLOCKED => "blocked",
                    EXIT_LEASE_BUSY => "lease_busy",
                    EXIT_RECOVERY_REQUIRED => "recovery_required",
                    EXIT_VERIFY_MISMATCH => "verify_mismatch",
                    EXIT_CANCELLED => "cancelled",
                    _ => "error",
                },
                "result_type": command.replace('.', "_"),
                "correlation": {
                    "logical_run_id": Value::Null,
                    "attempt_id": Value::Null,
                    "subject_id": Value::Null,
                },
                "counts": {},
                "diagnostics": [{
                    "code": failure.code,
                    "severity": if failure.exit_code == EXIT_INTERNAL { "error" } else { "warning" },
                    "phase": command,
                    "message_key": failure.code,
                    "correlation_id": Value::Null,
                    "context": {},
                }],
                "result": Value::Null,
                "data": Value::Null,
                "error": {
                    "code": failure.code,
                    "message": failure.message,
                },
            }))
            .expect("JSON envelope serialization cannot fail")
        ),
    }
    failure.exit_code
}

fn contract_context(data: &Value) -> (Value, Value) {
    let object = data.as_object();
    let string_field = |names: &[&str]| {
        object.and_then(|object| {
            names
                .iter()
                .find_map(|name| object.get(*name).and_then(Value::as_str))
        })
    };
    let logical_run_id = string_field(&["root_scan_id", "scan_run_id", "plan_run_id", "run_id"]);
    let attempt_id = string_field(&[
        "execution_run_id",
        "verify_run_id",
        "rollback_run_id",
        "recovery_run_id",
        "archive_id",
        "retention_run_id",
    ]);
    let subject_id = string_field(&[
        "subject_id",
        "operation_id",
        "execution_run_id",
        "parent_plan_run_id",
        "plan_run_id",
        "root_scan_id",
        "run_id",
    ]);
    let mut counts = serde_json::Map::new();
    if let Some(object) = object {
        for name in [
            "files",
            "items",
            "success",
            "skipped",
            "failed",
            "warnings",
            "conflicts",
            "risks",
            "plans",
            "executions",
            "logs",
            "record_count",
            "byte_count",
        ] {
            if let Some(value) = object.get(name).filter(|value| value.is_number()) {
                counts.insert(name.to_owned(), value.clone());
            }
        }
    }
    (
        json!({
            "logical_run_id": logical_run_id,
            "attempt_id": attempt_id,
            "subject_id": subject_id,
        }),
        Value::Object(counts),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_categories_have_stable_exit_codes() {
        assert_eq!(CommandResult::success(Value::Null, "ok").exit_code, 0);
        assert_eq!(CommandResult::blocked(Value::Null, "blocked").exit_code, 3);
        assert_eq!(CommandResult::partial(Value::Null, "partial").exit_code, 4);
        assert_eq!(
            CommandResult::recovery_required(Value::Null, "recovery").exit_code,
            7
        );
    }

    #[test]
    fn workflow_error_categories_are_stable() {
        assert_eq!(
            CliFailure::from_code("target_already_exists").exit_code,
            EXIT_BLOCKED
        );
        assert_eq!(
            CliFailure::from_code("recovery_required:operation-1").exit_code,
            EXIT_RECOVERY_REQUIRED
        );
        assert_eq!(
            CliFailure::from_code("source_delete_failed:access_denied").exit_code,
            EXIT_RECOVERY_REQUIRED
        );
        assert_eq!(
            CliFailure::from_code("mutation_scope_busy").exit_code,
            EXIT_LEASE_BUSY
        );
        assert_eq!(
            CliFailure::from_code("verify_target_content_mismatch").exit_code,
            EXIT_VERIFY_MISMATCH
        );
        assert_eq!(
            CliFailure::from_code("scan_cancelled").exit_code,
            EXIT_CANCELLED
        );
        assert_eq!(
            CliFailure::from_code("invalid_naming_rules:empty_required").exit_code,
            EXIT_USAGE
        );
        assert_eq!(
            CliFailure::from_code("database mutex poisoned").exit_code,
            EXIT_INTERNAL
        );
    }
}
