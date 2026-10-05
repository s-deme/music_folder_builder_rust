mod args;
mod config;
mod doctor;
mod output;

use args::{
    Cli, Command, CompletionShell, DiagnosticsCommand, EventFormat, HistoryCommand, OutputFormat,
    PlanCommand, PlanCreateArgs, RecoveryCommand,
};
use clap::{error::ErrorKind, Parser};
use music_folder_core::{
    ports::ManualTargetChange,
    usecases::{
        ApplyUseCase, CancellationToken, PlanOptions, PlanUseCase, RecoveryUseCase,
        RevisePlanUseCase, RollbackUseCase, ScanOptions, ScanProgress, ScanUseCase, VerifyUseCase,
    },
    NamingRules,
};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::LocalFileSystem,
};
use output::{emit_failure, emit_result, CliFailure, CommandResult};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static CLI_CANCELLATION: OnceLock<CancellationToken> = OnceLock::new();
static CLI_RUNTIME: OnceLock<CliRuntime> = OnceLock::new();

struct CliRuntime {
    output: OutputFormat,
    yes: bool,
    events: Option<CliEventStream>,
}

struct CliEventStream {
    command: String,
    correlation_id: String,
    next_sequence: AtomicU64,
    writer: Mutex<()>,
}

fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let requested_output = args::requested_output(&arguments);
    let requested_events = args::requested_events(&arguments);
    let command_hint = args::command_hint(&arguments);
    let cli = match Cli::try_parse_from(&arguments) {
        Ok(cli) => cli,
        Err(error) => {
            initialize_cli_runtime(requested_output, false, requested_events, &command_hint);
            let is_information = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
            if is_information {
                let text = error.to_string();
                emit_cli_event(
                    "terminal",
                    json!({ "terminal": true, "status": "success", "exit_code": 0 }),
                );
                let exit = if requested_output == OutputFormat::Json {
                    emit_result(
                        requested_output,
                        &command_hint,
                        CommandResult::success(json!({ "text": text }), text),
                    )
                } else {
                    print!("{text}");
                    0
                };
                std::process::exit(exit);
            }
            emit_cli_event(
                "terminal",
                json!({
                    "terminal": true,
                    "status": "error",
                    "error_code": "usage_error",
                    "exit_code": output::EXIT_USAGE,
                }),
            );
            let exit = if requested_output == OutputFormat::Json {
                emit_failure(
                    requested_output,
                    &command_hint,
                    CliFailure::usage("usage_error", error.to_string()),
                )
            } else {
                let _ = error.print();
                output::EXIT_USAGE
            };
            std::process::exit(exit);
        }
    };

    let format = cli.output;
    let command = cli.command.name();
    initialize_cli_runtime(format, cli.yes, cli.events, command);
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .try_init();
    if let Err(error) = install_interrupt_handler() {
        emit_cli_event(
            "terminal",
            json!({
                "terminal": true,
                "status": "error",
                "error_code": "interrupt_handler_install_failed",
                "exit_code": output::EXIT_INTERNAL,
            }),
        );
        let exit = emit_failure(
            cli.output,
            cli.command.name(),
            CliFailure::internal("interrupt_handler_install_failed", error),
        );
        std::process::exit(exit);
    }
    let exit = match execute(cli.command) {
        Ok(result) => {
            emit_cli_event(
                "terminal",
                json!({
                    "terminal": true,
                    "status": result.status,
                    "exit_code": result.exit_code,
                }),
            );
            emit_result(format, command, result)
        }
        Err(error) => {
            let event_status = match error.exit_code {
                output::EXIT_BLOCKED => "blocked",
                output::EXIT_PARTIAL => "partial",
                output::EXIT_LEASE_BUSY => "lease_busy",
                output::EXIT_RECOVERY_REQUIRED => "recovery_required",
                output::EXIT_VERIFY_MISMATCH => "verify_mismatch",
                output::EXIT_CANCELLED => "cancelled",
                _ => "error",
            };
            emit_cli_event(
                "terminal",
                json!({
                    "terminal": true,
                    "status": event_status,
                    "error_code": error.code.as_str(),
                    "exit_code": error.exit_code,
                }),
            );
            emit_failure(format, command, error)
        }
    };
    std::process::exit(exit);
}

fn initialize_cli_runtime(
    output: OutputFormat,
    yes: bool,
    events: Option<EventFormat>,
    command: &str,
) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stream = events.map(|EventFormat::Jsonl| CliEventStream {
        command: command.to_owned(),
        correlation_id: format!("cli-{}-{timestamp}", std::process::id()),
        next_sequence: AtomicU64::new(1),
        writer: Mutex::new(()),
    });
    let _ = CLI_RUNTIME.set(CliRuntime {
        output,
        yes,
        events: stream,
    });
}

fn emit_cli_event(event_type: &str, payload: Value) {
    let Some(stream) = CLI_RUNTIME
        .get()
        .and_then(|runtime| runtime.events.as_ref())
    else {
        return;
    };
    let sequence = stream.next_sequence.fetch_add(1, Ordering::Relaxed);
    let event = json!({
        "schema_version": 1,
        "event_type": event_type,
        "sequence": sequence,
        "correlation_id": stream.correlation_id.as_str(),
        "command": stream.command.as_str(),
        "payload": payload,
    });
    if let Ok(_guard) = stream.writer.lock() {
        eprintln!(
            "{}",
            serde_json::to_string(&event).unwrap_or_else(|_| {
                format!(
                    "{{\"schema_version\":1,\"event_type\":\"serialization_error\",\"sequence\":{sequence}}}"
                )
            })
        );
    }
}

fn confirm_destructive(action: &str) -> Result<(), CliFailure> {
    let runtime = CLI_RUNTIME.get();
    if runtime.is_some_and(|value| value.yes) {
        return Ok(());
    }
    if runtime.is_some_and(|value| value.output == OutputFormat::Json)
        || !std::io::stdin().is_terminal()
    {
        return Err(CliFailure::blocked(
            "non_interactive_confirmation_required",
            format!("{action} requires --yes when no interactive TTY is available"),
        ));
    }
    eprint!("{action}. Continue? [y/N] ");
    std::io::stderr()
        .flush()
        .map_err(|error| CliFailure::internal("confirmation_prompt_failed", error.to_string()))?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| CliFailure::internal("confirmation_read_failed", error.to_string()))?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliFailure::blocked(
            "destructive_command_cancelled",
            format!("{action} was not confirmed"),
        ))
    }
}

fn execute(command: Command) -> Result<CommandResult, CliFailure> {
    match command {
        Command::Doctor { command } => doctor::execute(command),
        Command::Scan {
            source,
            db,
            workers,
        } => scan(source, db, workers),
        Command::Plan { command, create } => match command {
            Some(PlanCommand::Revise {
                plan_run_id,
                db,
                changes,
                manual,
                candidates,
            }) => revise_plan(plan_run_id, db, changes, manual, candidates),
            None => create_plan(*create),
        },
        Command::Apply {
            plan_run_id,
            db,
            execute,
            confirm,
        } => apply(plan_run_id, db, execute, confirm),
        Command::Verify {
            execution_run_id,
            subject,
            subject_id,
            db,
        } => {
            let selected_id = subject_id.or(execution_run_id).ok_or_else(|| {
                CliFailure::usage("verify_subject_required", "verify requires an attempt ID")
            })?;
            verify(selected_id, subject, db)
        }
        Command::Rollback {
            execution_run_id,
            db,
            execute,
            confirm,
        } => rollback(execution_run_id, db, execute, confirm),
        Command::History { command } => history(command),
        Command::Recovery { command } => recovery(command),
        Command::Diagnostics { command } => diagnostics(command),
        Command::Benchmark {
            source,
            db,
            target,
            workers,
            iterations,
        } => benchmark(source, db, target, workers, iterations),
        Command::Completions { shell } => Ok(completions(shell)),
        Command::Man => Ok(command_reference()),
    }
}

fn diagnostics(command: DiagnosticsCommand) -> Result<CommandResult, CliFailure> {
    match command {
        DiagnosticsCommand::Export {
            db,
            destination,
            include_sensitive_paths,
        } => {
            let exported = open_store(&db)?
                .export_diagnostics(&destination, !include_sensitive_paths)
                .map_err(|error| CliFailure::from_code(&error))?;
            let human = format!(
                "diagnostics exported records={} bytes={} sha256={} path={}",
                exported.record_count,
                exported.byte_count,
                exported.sha256,
                exported.export_path.display()
            );
            let mut data = serde_json::to_value(exported).map_err(|error| {
                CliFailure::internal("json_serialize_failed", error.to_string())
            })?;
            data["export_path"] = path_envelope(&destination, "diagnostic_export");
            Ok(CommandResult::success(data, human))
        }
        DiagnosticsCommand::Retention {
            db,
            progress_debug_days,
            diagnostic_days,
            recovery_audit_days,
            batch_size,
            max_database_bytes,
            execute,
        } => {
            if execute {
                confirm_destructive("Delete eligible diagnostic history")?;
            }
            let policy = music_folder_infra::sqlite::DiagnosticRetentionPolicy {
                progress_debug_days,
                diagnostic_days,
                recovery_audit_days,
                batch_size,
                max_database_bytes,
            };
            let store = open_store(&db)?;
            if execute {
                let result = store
                    .run_diagnostic_retention(&policy)
                    .map_err(|error| CliFailure::from_code(&error))?;
                let human = format!(
                    "diagnostic retention deleted progress/debug={} diagnostic={} recovery/audit={} protected={} capacity_exceeded={}",
                    result.deleted_progress_debug,
                    result.deleted_diagnostic,
                    result.deleted_recovery_audit,
                    result.protected_remaining,
                    result.capacity_exceeded
                );
                Ok(CommandResult::success(
                    serde_json::to_value(result).map_err(|error| {
                        CliFailure::internal("json_serialize_failed", error.to_string())
                    })?,
                    human,
                ))
            } else {
                let preview = store
                    .preview_diagnostic_retention(&policy)
                    .map_err(|error| CliFailure::from_code(&error))?;
                let human = format!(
                    "diagnostic retention dry-run eligible progress/debug={} diagnostic={} recovery/audit={} protected={}",
                    preview.eligible_progress_debug,
                    preview.eligible_diagnostic,
                    preview.eligible_recovery_audit,
                    preview.protected_remaining
                );
                Ok(CommandResult::success(
                    json!({ "dry_run": true, "policy": policy, "preview": preview }),
                    human,
                ))
            }
        }
    }
}

fn open_store(path: &Path) -> Result<Arc<SqliteScanStore>, CliFailure> {
    SqliteScanStore::open(path)
        .map(Arc::new)
        .map_err(|error| CliFailure::internal("database_open_failed", error))
}

fn scan(source: PathBuf, db: PathBuf, workers: Option<usize>) -> Result<CommandResult, CliFailure> {
    let store = open_store(&db)?;
    let mut options = scan_options(workers);
    attach_scan_progress(&mut options);
    let result = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store,
    }
    .execute(&source, &options)
    .map_err(CliFailure::from_workflow)?;
    if options.cancellation.is_cancelled() {
        return Err(CliFailure::from_code("scan_cancelled"));
    }
    let data = json!({
        "scan_run_id": result.scan_id,
        "files": result.files,
        "cache_hits": result.cache_hits,
        "warnings": result.warnings,
    });
    Ok(CommandResult::success(
        data,
        format!(
            "scan_run_id={} files={} cache_hits={} warnings={}",
            result.scan_id, result.files, result.cache_hits, result.warnings
        ),
    ))
}

fn create_plan(args: PlanCreateArgs) -> Result<CommandResult, CliFailure> {
    let scan_run_id = args.scan_run_id.as_deref().ok_or_else(|| {
        CliFailure::usage(
            "plan_create_arguments_required",
            "plan creation requires --scan-run-id and --target",
        )
    })?;
    let target = args.target.as_ref().ok_or_else(|| {
        CliFailure::usage(
            "plan_create_arguments_required",
            "plan creation requires --scan-run-id and --target",
        )
    })?;
    let naming = config::load_naming(&args)?;
    let effective_naming = naming.clone();
    let store = open_store(&args.db)?;
    let result = PlanUseCase { store }
        .execute(
            scan_run_id,
            &PlanOptions {
                target_root: target.clone(),
                batch_size: 250,
                naming,
            },
        )
        .map_err(CliFailure::from_workflow)?;
    let data = json!({
        "plan_run_id": result.plan_id,
        "items": result.items,
        "conflicts": result.conflicts,
        "risks": result.risks,
        "naming": effective_naming,
    });
    let human = format!(
        "plan_run_id={} items={} conflicts={} risks={}",
        result.plan_id, result.items, result.conflicts, result.risks
    );
    if result.conflicts > 0 {
        Ok(CommandResult::blocked(data, human))
    } else {
        Ok(CommandResult::success(data, human))
    }
}

fn revise_plan(
    plan_run_id: String,
    db: PathBuf,
    legacy_changes: Vec<args::TargetChange>,
    manual: Vec<std::ffi::OsString>,
    candidates: Vec<args::CandidateSelection>,
) -> Result<CommandResult, CliFailure> {
    let manual_count = manual.len() / 2;
    let candidate_count = candidates.len();
    let change_count = legacy_changes.len() + manual_count + candidate_count;
    if change_count == 0 {
        return Err(CliFailure::usage(
            "manual_target_change_required",
            "plan revise requires --manual, --candidate, or compatibility --change",
        ));
    }
    if change_count > 100 {
        return Err(CliFailure::usage(
            "plan_revision_change_limit",
            "a single Plan revision accepts at most 100 changes",
        ));
    }
    let store = open_store(&db)?;
    let mut seen_items = HashSet::with_capacity(change_count);
    let mut changes = Vec::with_capacity(change_count);
    for change in legacy_changes {
        if !seen_items.insert(change.item_id.clone()) {
            return Err(CliFailure::usage(
                "plan_revision_duplicate_item",
                "each Plan item may be changed only once per revision",
            ));
        }
        let target = store
            .validate_manual_target_for_plan(&plan_run_id, &change.item_id, &change.target)
            .map_err(|error| CliFailure::from_code(&error))?;
        changes.push(ManualTargetChange {
            plan_item_id: change.item_id,
            target,
            reason: "cli_manual_target_compat".into(),
        });
    }
    for pair in manual.chunks_exact(2) {
        let item_id = pair[0].to_str().ok_or_else(|| {
            CliFailure::usage(
                "plan_item_id_invalid",
                "Plan item IDs must use the stable UTF-8 identifier form",
            )
        })?;
        let item_id = args::parse_identifier(item_id)
            .map_err(|error| CliFailure::usage("plan_item_id_invalid", error))?;
        if !seen_items.insert(item_id.clone()) {
            return Err(CliFailure::usage(
                "plan_revision_duplicate_item",
                "each Plan item may be changed only once per revision",
            ));
        }
        let target = PathBuf::from(&pair[1]);
        let target = store
            .validate_manual_target_for_plan(&plan_run_id, &item_id, &target)
            .map_err(|error| CliFailure::from_code(&error))?;
        changes.push(ManualTargetChange {
            plan_item_id: item_id,
            target,
            reason: "cli_manual_target".into(),
        });
    }
    for candidate in candidates {
        if !seen_items.insert(candidate.item_id.clone()) {
            return Err(CliFailure::usage(
                "plan_revision_duplicate_item",
                "each Plan item may be changed only once per revision",
            ));
        }
        let target = store
            .resolve_plan_conflict_candidate_target(
                &plan_run_id,
                &candidate.item_id,
                &candidate.conflict_group_id,
                candidate.ordinal,
            )
            .map_err(|error| CliFailure::from_code(&error))?;
        changes.push(ManualTargetChange {
            plan_item_id: candidate.item_id,
            target,
            reason: format!("cli_conflict_candidate:{}", candidate.ordinal),
        });
    }
    let child_plan_id = RevisePlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(&plan_run_id, &changes)
    .map_err(CliFailure::from_workflow)?;
    Ok(CommandResult::success(
        json!({
            "parent_plan_run_id": plan_run_id,
            "plan_run_id": child_plan_id,
            "changes": changes.len(),
            "manual_changes": changes.len() - candidate_count,
            "candidate_changes": candidate_count,
        }),
        format!(
            "plan_run_id={} parent_plan_run_id={} changes={}",
            child_plan_id,
            plan_run_id,
            changes.len()
        ),
    ))
}

fn apply(
    plan_run_id: String,
    db: PathBuf,
    execute: bool,
    confirm: Option<String>,
) -> Result<CommandResult, CliFailure> {
    if execute && confirm.as_deref() != Some(plan_run_id.as_str()) {
        return Err(CliFailure::blocked(
            "apply_confirmation_required",
            "--execute requires --confirm with the exact Plan run ID",
        ));
    }
    if execute {
        confirm_destructive(&format!("Apply persisted Plan {plan_run_id}"))?;
    }
    let store = open_store(&db)?;
    let result = ApplyUseCase {
        store: store.clone(),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan_run_id, !execute)
    .map_err(CliFailure::from_workflow)?;
    let mode = if execute { "apply" } else { "dry_run" };
    let recovery_operation_ids = if execute && result.failed > 0 {
        recovery_operations_for_attempt(store, &result.execution_id)?
    } else {
        Vec::new()
    };
    let recovery_required = !recovery_operation_ids.is_empty();
    let data = json!({
        "execution_run_id": result.execution_id,
        "plan_run_id": plan_run_id,
        "mode": mode,
        "success": result.success,
        "skipped": result.skipped,
        "failed": result.failed,
        "recovery_operation_ids": recovery_operation_ids,
    });
    let human = format!(
        "execution_run_id={} mode={} success={} skipped={} failed={}",
        result.execution_id, mode, result.success, result.skipped, result.failed
    );
    if recovery_required {
        Ok(CommandResult::recovery_required(data, human))
    } else if execute && result.failed > 0 {
        Ok(CommandResult::partial(data, human))
    } else if result.failed > 0 || result.skipped > 0 {
        Ok(CommandResult::blocked(data, human))
    } else {
        Ok(CommandResult::success(data, human))
    }
}

fn verify(
    subject_id: String,
    subject: args::VerifySubjectArg,
    db: PathBuf,
) -> Result<CommandResult, CliFailure> {
    let subject_kind = match subject {
        args::VerifySubjectArg::Execution => music_folder_core::VerifySubject::Execution,
        args::VerifySubjectArg::Rollback => music_folder_core::VerifySubject::Rollback,
        args::VerifySubjectArg::Recovery => music_folder_core::VerifySubject::Recovery,
    };
    let result = VerifyUseCase {
        store: open_store(&db)?,
        files: Arc::new(LocalFileSystem),
    }
    .execute_subject(&subject_id, subject_kind)
    .map_err(CliFailure::from_workflow)?;
    let data = json!({
        "verify_run_id": result.verify_id,
        "subject_kind": subject.as_str(),
        "subject_id": subject_id.as_str(),
        "execution_run_id": if matches!(subject, args::VerifySubjectArg::Execution) {
            Some(subject_id.as_str())
        } else {
            None
        },
        "success": result.success,
        "failed": result.failed,
    });
    let human = format!(
        "verify_run_id={} subject_kind={} subject_id={} success={} failed={}",
        result.verify_id,
        subject.as_str(),
        subject_id,
        result.success,
        result.failed
    );
    if result.failed > 0 {
        Ok(CommandResult::verify_mismatch(data, human))
    } else {
        Ok(CommandResult::success(data, human))
    }
}

fn rollback(
    execution_run_id: String,
    db: PathBuf,
    execute: bool,
    confirm: Option<String>,
) -> Result<CommandResult, CliFailure> {
    if execute && confirm.as_deref() != Some(execution_run_id.as_str()) {
        return Err(CliFailure::blocked(
            "rollback_confirmation_required",
            "--execute requires --confirm with the exact execution run ID",
        ));
    }
    if execute {
        confirm_destructive(&format!("Rollback execution {execution_run_id}"))?;
    }
    let store = open_store(&db)?;
    let result = RollbackUseCase {
        store: store.clone(),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&execution_run_id, !execute)
    .map_err(CliFailure::from_workflow)?;
    let mode = if execute { "rollback" } else { "dry_run" };
    let recovery_operation_ids = if execute && result.failed > 0 {
        recovery_operations_for_attempt(store, &result.rollback_id)?
    } else {
        Vec::new()
    };
    let recovery_required = !recovery_operation_ids.is_empty();
    let data = json!({
        "rollback_run_id": result.rollback_id,
        "execution_run_id": execution_run_id,
        "mode": mode,
        "success": result.success,
        "skipped": result.skipped,
        "failed": result.failed,
        "recovery_operation_ids": recovery_operation_ids,
    });
    let human = format!(
        "rollback_run_id={} execution_run_id={} mode={} success={} skipped={} failed={}",
        result.rollback_id, execution_run_id, mode, result.success, result.skipped, result.failed
    );
    if recovery_required {
        Ok(CommandResult::recovery_required(data, human))
    } else if execute && result.failed > 0 {
        Ok(CommandResult::partial(data, human))
    } else if result.failed > 0 || result.skipped > 0 {
        Ok(CommandResult::blocked(data, human))
    } else {
        Ok(CommandResult::success(data, human))
    }
}

fn recovery_operations_for_attempt(
    store: Arc<SqliteScanStore>,
    attempt_id: &str,
) -> Result<Vec<String>, CliFailure> {
    RecoveryUseCase {
        store,
        files: Arc::new(LocalFileSystem),
    }
    .inspect()
    .map(|items| {
        items
            .into_iter()
            .filter(|item| item.attempt_id == attempt_id)
            .map(|item| item.operation_id)
            .collect()
    })
    .map_err(CliFailure::from_workflow)
}

fn history(command: HistoryCommand) -> Result<CommandResult, CliFailure> {
    match command {
        HistoryCommand::List {
            db,
            limit,
            kind,
            status,
            query,
            oldest_first,
        } => {
            let store = open_store(&db)?;
            let rows = store
                .list_history_filtered(
                    limit.clamp(1, 200),
                    None,
                    None,
                    kind.map(args::RunKind::as_str),
                    status.map(args::HistoryStatus::as_str),
                    query.as_deref(),
                    oldest_first,
                )
                .map_err(|error| CliFailure::from_code(&error))?;
            let count = rows.len();
            Ok(CommandResult::success(
                serde_json::to_value(rows).map_err(|error| {
                    CliFailure::internal("json_serialize_failed", error.to_string())
                })?,
                format!("history rows={count}"),
            ))
        }
        HistoryCommand::CleanupPreview { db, kind, run_id } => {
            let preview = open_store(&db)?
                .history_cleanup_preview(kind.as_str(), &run_id)
                .map_err(|error| CliFailure::from_code(&error))?;
            let mut data = serde_json::to_value(&preview).map_err(|error| {
                CliFailure::internal("json_serialize_failed", error.to_string())
            })?;
            data["archive_path"] = path_envelope(&preview.archive_path, "history_archive");
            let human = format!(
                "run_id={} plans={} executions={} logs={} blocked={}",
                run_id, preview.plans, preview.executions, preview.logs, preview.blocked
            );
            if preview.blocked {
                Ok(CommandResult::blocked(data, human))
            } else {
                Ok(CommandResult::success(data, human))
            }
        }
        HistoryCommand::Archive {
            db,
            kind,
            run_id,
            archive_dir,
            confirm,
        } => {
            if confirm.as_deref() != Some(run_id.as_str()) {
                return Err(CliFailure::blocked(
                    "history_archive_confirmation_required",
                    "history archive requires --confirm with the exact run ID",
                ));
            }
            confirm_destructive(&format!("Archive workflow {run_id}"))?;
            let store = open_store(&db)?;
            let preview = store
                .history_cleanup_preview(kind.as_str(), &run_id)
                .map_err(|error| CliFailure::from_code(&error))?;
            if preview.blocked {
                return Err(CliFailure::blocked(
                    "history_archive_blocked",
                    format!(
                        "workflow archive is protected: {}",
                        preview.protection_reasons.join(",")
                    ),
                ));
            }
            let result = store
                .archive_history(kind.as_str(), &run_id, archive_dir.as_deref())
                .map_err(|error| CliFailure::from_code(&error))?;
            let human = format!(
                "archived {} workflow root_scan_id={} records={} bytes={} sha256={}",
                kind.as_str(),
                result.root_scan_id,
                result.record_count,
                result.byte_count,
                result.sha256
            );
            let mut data = serde_json::to_value(&result).map_err(|error| {
                CliFailure::internal("json_serialize_failed", error.to_string())
            })?;
            data["archive_path"] = path_envelope(&result.archive_path, "history_archive");
            Ok(CommandResult::success(data, human))
        }
        HistoryCommand::Delete {
            db,
            kind,
            run_id,
            confirm,
        } => {
            if !matches!(kind, args::RunKind::Scan) {
                return Err(CliFailure::usage(
                    "history_purge_requires_root_scan",
                    "history delete accepts only --kind scan",
                ));
            }
            if confirm.as_deref() != Some(run_id.as_str()) {
                return Err(CliFailure::blocked(
                    "history_delete_confirmation_required",
                    "history delete requires --confirm with the exact run ID",
                ));
            }
            confirm_destructive(&format!("Permanently delete archived workflow {run_id}"))?;
            let store = open_store(&db)?;
            let preview = store
                .history_cleanup_preview(kind.as_str(), &run_id)
                .map_err(|error| CliFailure::from_code(&error))?;
            if preview.blocked {
                return Err(CliFailure::blocked(
                    "history_cleanup_blocked",
                    "running dependent history cannot be deleted",
                ));
            }
            store
                .delete_history(kind.as_str(), &run_id)
                .map_err(|error| CliFailure::from_code(&error))?;
            Ok(CommandResult::success(
                json!({
                    "run_id": run_id,
                    "kind": kind.as_str(),
                    "deleted": true,
                    "plans": preview.plans,
                    "executions": preview.executions,
                    "logs": preview.logs,
                }),
                format!("deleted {} history run_id={}", kind.as_str(), run_id),
            ))
        }
    }
}

fn recovery(command: RecoveryCommand) -> Result<CommandResult, CliFailure> {
    match command {
        RecoveryCommand::List { db } => {
            let usecase = RecoveryUseCase {
                store: open_store(&db)?,
                files: Arc::new(LocalFileSystem),
            };
            let items = usecase.inspect().map_err(CliFailure::from_workflow)?;
            let data = Value::Array(
                items
                    .iter()
                    .map(|item| {
                        json!({
                            "operation_id": item.operation_id,
                            "attempt_id": item.attempt_id,
                            "sequence_no": item.sequence_no,
                            "state": item.state,
                            "source": path_envelope(&item.source, "source"),
                            "target": path_envelope(&item.target, "target"),
                            "temporary": path_envelope(&item.temporary, "temporary"),
                            "expected_size": item.expected_size,
                            "expected_content_sha256": item.expected_content_sha256,
                            "suggested_action": item.suggested_action,
                        })
                    })
                    .collect(),
            );
            if items.is_empty() {
                Ok(CommandResult::success(data, "recovery items=0"))
            } else {
                Ok(CommandResult::recovery_required(
                    data,
                    format!("recovery items={}", items.len()),
                ))
            }
        }
        RecoveryCommand::Run {
            db,
            operation_id,
            action,
            execute,
            confirm,
        } => {
            if execute && confirm.as_deref() != Some(operation_id.as_str()) {
                return Err(CliFailure::blocked(
                    "recovery_confirmation_required",
                    "recovery --execute requires --confirm with the exact operation ID",
                ));
            }
            if execute {
                confirm_destructive(&format!("Recover operation {operation_id}"))?;
            }
            let store = open_store(&db)?;
            let result = RecoveryUseCase {
                store: store.clone(),
                files: Arc::new(LocalFileSystem),
            }
            .execute(&operation_id, action.map(Into::into), !execute)
            .map_err(CliFailure::from_workflow);
            let result = match result {
                Ok(result) => result,
                Err(failure) if failure.exit_code == output::EXIT_INTERNAL => {
                    let still_pending = RecoveryUseCase {
                        store,
                        files: Arc::new(LocalFileSystem),
                    }
                    .inspect()
                    .map(|items| items.iter().any(|item| item.operation_id == operation_id))
                    .unwrap_or(true);
                    if still_pending {
                        return Err(CliFailure::recovery_required(failure.code, failure.message));
                    }
                    return Err(failure);
                }
                Err(failure) => return Err(failure),
            };
            let data = json!({
                "recovery_run_id": result.recovery_id,
                "operation_id": result.operation_id,
                "action": result.action,
                "mode": if result.dry_run { "dry_run" } else { "recovery" },
            });
            let human = format!(
                "recovery_run_id={} operation_id={} action={:?} mode={}",
                result.recovery_id,
                result.operation_id,
                result.action,
                if result.dry_run {
                    "dry_run"
                } else {
                    "recovery"
                }
            );
            if result.dry_run {
                Ok(CommandResult::recovery_required(data, human))
            } else {
                Ok(CommandResult::success(data, human))
            }
        }
    }
}

fn benchmark(
    source: PathBuf,
    db: PathBuf,
    target: Option<PathBuf>,
    workers: Option<usize>,
    iterations: u32,
) -> Result<CommandResult, CliFailure> {
    let options = scan_options(workers);
    let workspace = BenchmarkWorkspace::new(&db)
        .map_err(|error| CliFailure::internal("benchmark_workspace_failed", error))?;
    let target_root =
        std::path::absolute(target.unwrap_or_else(|| workspace.path().join("target")))
            .map_err(|error| CliFailure::usage("benchmark_target_invalid", error.to_string()))?;
    let rate = |files: u64, elapsed_ms: u64| {
        if elapsed_ms == 0 {
            files as f64
        } else {
            files as f64 * 1000.0 / elapsed_ms as f64
        }
    };
    let baseline_rss_bytes = current_rss_bytes();
    let mut reports = Vec::with_capacity(iterations as usize);
    let mut cold_times = Vec::with_capacity(iterations as usize);
    let mut warm_times = Vec::with_capacity(iterations as usize);
    let mut plan_times = Vec::with_capacity(iterations as usize);
    let mut apply_times = Vec::with_capacity(iterations as usize);
    let mut peak_samples = Vec::with_capacity(iterations as usize);
    let mut cold_files = None;
    let mut warm_files = None;
    let mut warm_cache_hits = Vec::with_capacity(iterations as usize);
    for iteration in 1..=iterations {
        let iteration_db = workspace.path().join(format!("iteration-{iteration}.db"));
        let store = open_store(&iteration_db)?;
        let sampler = PeakRssSampler::start();
        let run_scan = || -> Result<_, CliFailure> {
            let started = Instant::now();
            ScanUseCase {
                fs: Arc::new(LocalFileSystem),
                metadata: Arc::new(LoftyMetadataReader),
                store: Arc::clone(&store),
            }
            .execute(&source, &options)
            .map(|result| (result, started.elapsed().as_millis() as u64))
            .map_err(CliFailure::from_workflow)
        };
        let (cold, cold_ms) = run_scan()?;
        let (warm, warm_ms) = run_scan()?;
        if cold_files
            .replace(cold.files)
            .is_some_and(|files| files != cold.files)
            || warm_files
                .replace(warm.files)
                .is_some_and(|files| files != warm.files)
        {
            return Err(CliFailure::internal(
                "benchmark_corpus_changed",
                "file count changed between benchmark iterations",
            ));
        }
        let naming = NamingRules {
            allow_missing_metadata: true,
            ..NamingRules::default()
        };
        let plan_started = Instant::now();
        let plan = PlanUseCase {
            store: Arc::clone(&store),
        }
        .execute(
            &warm.scan_id,
            &PlanOptions {
                target_root: target_root.join(format!("iteration-{iteration}")),
                batch_size: options.batch_size.max(1),
                naming,
            },
        )
        .map_err(CliFailure::from_workflow)?;
        let plan_ms = plan_started.elapsed().as_millis() as u64;
        let apply_started = Instant::now();
        let apply = ApplyUseCase {
            store: Arc::clone(&store),
            files: Arc::new(LocalFileSystem),
        }
        .execute(&plan.plan_id, true)
        .map_err(CliFailure::from_workflow)?;
        let apply_ms = apply_started.elapsed().as_millis() as u64;
        let peak_rss_bytes = sampler.finish();
        let phases = |run_id: &str| {
            store
                .list_metrics(run_id)
                .map_err(|error| CliFailure::from_code(&error))
        };
        cold_times.push(cold_ms);
        warm_times.push(warm_ms);
        plan_times.push(plan_ms);
        apply_times.push(apply_ms);
        peak_samples.push(peak_rss_bytes);
        warm_cache_hits.push(warm.cache_hits);
        reports.push(json!({
            "iteration": iteration,
            "cold": {
                "scan_id": cold.scan_id,
                "files": cold.files,
                "cache_hits": cold.cache_hits,
                "tag_reads": cold.files.saturating_sub(cold.cache_hits),
                "elapsed_ms": cold_ms,
                "items_per_second": rate(cold.files, cold_ms),
                "phases": phases(&cold.scan_id)?,
            },
            "warm": {
                "scan_id": warm.scan_id,
                "files": warm.files,
                "cache_hits": warm.cache_hits,
                "tag_reads": warm.files.saturating_sub(warm.cache_hits),
                "cache_hit_rate": ratio(warm.cache_hits, warm.files),
                "elapsed_ms": warm_ms,
                "items_per_second": rate(warm.files, warm_ms),
                "phases": phases(&warm.scan_id)?,
            },
            "plan": {
                "plan_id": plan.plan_id,
                "items": plan.items,
                "conflicts": plan.conflicts,
                "risks": plan.risks,
                "elapsed_ms": plan_ms,
                "items_per_second": rate(plan.items, plan_ms),
            },
            "apply_dry_run": {
                "execution_id": apply.execution_id,
                "success": apply.success,
                "skipped": apply.skipped,
                "failed": apply.failed,
                "elapsed_ms": apply_ms,
                "items_per_second": rate(apply.success.saturating_add(apply.skipped).saturating_add(apply.failed), apply_ms),
            },
            "peak_rss_bytes": peak_rss_bytes,
        }));
    }
    let cold_ms = median(&mut cold_times);
    let warm_ms = median(&mut warm_times);
    let plan_ms = median(&mut plan_times);
    let apply_ms = median(&mut apply_times);
    let files = cold_files.unwrap_or(0);
    let warm_file_count = warm_files.unwrap_or(0);
    let median_cache_hits = median(&mut warm_cache_hits);
    let peak_rss_bytes = peak_samples.into_iter().max().unwrap_or(baseline_rss_bytes);
    let data = json!({
        "benchmark_schema_version": 2,
        "iterations": reports,
        "iteration_count": iterations,
        "cold": {
            "files": files,
            "elapsed_ms": cold_ms,
            "items_per_second": rate(files, cold_ms),
            "statistic": "median",
        },
        "warm": {
            "files": warm_file_count,
            "cache_hits": median_cache_hits,
            "cache_hit_rate": ratio(median_cache_hits, warm_file_count),
            "elapsed_ms": warm_ms,
            "items_per_second": rate(warm_file_count, warm_ms),
            "statistic": "median",
        },
        "plan": { "elapsed_ms": plan_ms, "items_per_second": rate(files, plan_ms), "statistic": "median" },
        "apply_dry_run": { "elapsed_ms": apply_ms, "items_per_second": rate(files, apply_ms), "statistic": "median" },
        "rss_bytes": peak_rss_bytes,
        "peak_rss_bytes": peak_rss_bytes,
        "baseline_rss_bytes": baseline_rss_bytes,
        "rss_growth_bytes": peak_rss_bytes.saturating_sub(baseline_rss_bytes),
        "rss_sampling_interval_ms": 5,
        "workspace_ephemeral": true,
    });
    Ok(CommandResult::success(
        data,
        format!(
            "benchmark iterations={} cold_median_ms={} warm_median_ms={} plan_median_ms={} apply_dry_run_median_ms={} peak_rss_bytes={}",
            iterations, cold_ms, warm_ms, plan_ms, apply_ms, peak_rss_bytes
        ),
    ))
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn path_envelope(path: &Path, role: &str) -> Value {
    json!(music_folder_infra::path_codec::path_envelope(path, role))
}

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

struct BenchmarkWorkspace {
    path: PathBuf,
}

impl BenchmarkWorkspace {
    fn new(database_hint: &Path) -> Result<Self, String> {
        let parent = database_hint
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for collision in 0..100_u32 {
            let path = parent.join(format!(
                ".music-folder-benchmark-{}-{nonce}-{collision}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("benchmark_workspace_collision".into())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BenchmarkWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct PeakRssSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}

impl PeakRssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(current_rss_bytes()));
        let thread_stop = Arc::clone(&stop);
        let thread_peak = Arc::clone(&peak);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                thread_peak.fetch_max(current_rss_bytes(), Ordering::AcqRel);
                thread::sleep(Duration::from_millis(5));
            }
            thread_peak.fetch_max(current_rss_bytes(), Ordering::AcqRel);
        });
        Self {
            stop,
            peak,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.peak
            .load(Ordering::Acquire)
            .max(process_peak_rss_bytes())
    }
}

impl Drop for PeakRssSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn scan_options(workers: Option<usize>) -> ScanOptions {
    let mut options = ScanOptions {
        cancellation: CLI_CANCELLATION.get().cloned().unwrap_or_default(),
        ..ScanOptions::default()
    };
    if let Some(value) = workers {
        options.workers = value.max(1);
    }
    options
}

fn install_interrupt_handler() -> Result<(), String> {
    let cancellation = CancellationToken::default();
    CLI_CANCELLATION
        .set(cancellation.clone())
        .map_err(|_| "interrupt_handler_already_installed".to_string())?;
    install_platform_interrupt_handler()?;
    #[cfg(debug_assertions)]
    schedule_fault_cancellation(cancellation)?;
    Ok(())
}

#[cfg(debug_assertions)]
fn schedule_fault_cancellation(cancellation: CancellationToken) -> Result<(), String> {
    let Some(raw) = std::env::var_os("MFB_FAULT_CANCEL_AFTER_MS") else {
        return Ok(());
    };
    let milliseconds = raw
        .to_str()
        .ok_or_else(|| "fault_cancel_delay_invalid".to_string())?
        .parse::<u64>()
        .map_err(|_| "fault_cancel_delay_invalid".to_string())?;
    if milliseconds > 60_000 {
        return Err("fault_cancel_delay_invalid".into());
    }
    thread::spawn(move || {
        if milliseconds > 0 {
            thread::sleep(Duration::from_millis(milliseconds));
        }
        cancellation.cancel();
    });
    Ok(())
}

#[cfg(unix)]
fn install_platform_interrupt_handler() -> Result<(), String> {
    use std::ffi::c_int;

    type SignalHandler = usize;
    const SIGINT: c_int = 2;
    const SIG_ERR: SignalHandler = usize::MAX;
    unsafe extern "C" {
        fn signal(signal: c_int, handler: SignalHandler) -> SignalHandler;
    }
    extern "C" fn cancel_on_signal(_signal: c_int) {
        if let Some(cancellation) = CLI_CANCELLATION.get() {
            cancellation.cancel();
        }
    }
    // SAFETY: `cancel_on_signal` has C signal-handler ABI, is process-static,
    // and only performs a lock-free cancellation flag store.
    let previous = unsafe { signal(SIGINT, cancel_on_signal as *const () as SignalHandler) };
    if previous == SIG_ERR {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn install_platform_interrupt_handler() -> Result<(), String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    unsafe extern "system" fn cancel_on_console_event(event: u32) -> i32 {
        const CTRL_C_EVENT: u32 = 0;
        const CTRL_BREAK_EVENT: u32 = 1;
        if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
            if let Some(cancellation) = CLI_CANCELLATION.get() {
                cancellation.cancel();
            }
            1
        } else {
            0
        }
    }
    // SAFETY: the callback is process-static and remains valid until exit.
    let installed = unsafe { SetConsoleCtrlHandler(Some(cancel_on_console_event), 1) };
    if installed == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn install_platform_interrupt_handler() -> Result<(), String> {
    Ok(())
}

fn attach_scan_progress(options: &mut ScanOptions) {
    options.progress = Some(Arc::new(|progress: ScanProgress| {
        if CLI_RUNTIME
            .get()
            .is_some_and(|runtime| runtime.events.is_some())
        {
            emit_cli_event(
                "progress",
                serde_json::to_value(&progress).unwrap_or_else(|_| {
                    json!({
                        "phase": "unknown",
                        "serialization_error": true,
                    })
                }),
            );
        } else {
            eprintln!(
                "progress phase={} processed={} enumerated={} cache_hits={} warnings={}",
                progress.phase,
                progress.processed,
                progress.enumerated,
                progress.cache_hits,
                progress.warnings
            );
        }
    }));
}

fn completions(shell: CompletionShell) -> CommandResult {
    const COMMANDS: &str =
        "scan plan apply verify rollback history recovery diagnostics doctor benchmark completions man";
    let script = match shell {
        CompletionShell::Bash => format!("complete -W '{COMMANDS}' music-folder"),
        CompletionShell::Zsh => format!(
            "#compdef music-folder\n_arguments '1:command:({})' '*::arguments:_normal'",
            COMMANDS
        ),
        CompletionShell::Fish => format!("complete -c music-folder -f -a '{}'", COMMANDS),
        CompletionShell::Powershell => format!(
            "Register-ArgumentCompleter -Native -CommandName music-folder -ScriptBlock {{ param($wordToComplete) @('{}') -split ' ' | Where-Object {{ $_ -like \"$wordToComplete*\" }} | ForEach-Object {{ [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }} }}",
            COMMANDS
        ),
    };
    CommandResult::success(
        json!({ "shell": format!("{shell:?}").to_lowercase(), "script": script }),
        script,
    )
}

fn command_reference() -> CommandResult {
    const MANUAL: &str = concat!(
        "music-folder(1)\n\nCOMMANDS\n",
        "  scan                 Create an immutable scan snapshot.\n",
        "  plan                 Create a Plan; accepts --config TOML and --allow-long-paths.\n",
        "  plan revise          Create a child Plan with lossless --manual ITEM PATH or opaque --candidate ITEM:GROUP:ORDINAL.\n",
        "  apply                Dry-run by default; --execute requires exact --confirm PLAN_ID.\n",
        "  verify               Verify an execution.\n",
        "  rollback             Dry-run by default; --execute requires exact --confirm.\n",
        "  history list         List persisted workflow history.\n",
        "  history cleanup-preview\n                       Preview archive closure and protection reasons.\n",
        "  history archive      Atomically create and verify a lossless JSONL archive.\n",
        "  history delete       Purge an archived root scan; requires exact --confirm RUN_ID.\n",
        "  recovery list        List non-terminal journal operations.\n",
        "  recovery run         Dry-run by default; --execute requires exact --confirm.\n",
        "  diagnostics export   Export schema-versioned diagnostics; paths are redacted by default.\n",
        "  doctor scan --source SOURCE [--db DB]\n                       Diagnose without modifying music; saves results and cache.\n",
        "  doctor show|issues|duplicates|albums --run-id ID [--db DB]\n                       Read saved diagnosis; issues accepts --severity and --code.\n",
        "  diagnostics retention\n                       Dry-run by default; --execute prunes a bounded eligible batch.\n",
        "  benchmark            Compare cold and warm scans.\n\nOUTPUT\n",
        "  --output human|json  JSON uses envelope schema_version 1. Progress is stderr-only.\n",
        "  --events jsonl       Stream versioned progress/terminal events to stderr.\n",
        "  --yes                Confirm destructive execution without a TTY prompt.\n\n",
        "EXIT STATUS\n",
        "  0 success, 2 usage/config, 3 blocked, 4 partial, 5 I/O or internal,\n",
        "  6 lease busy, 7 recovery required, 8 verify mismatch, 9 cancelled.",
    );
    CommandResult::success(json!({ "manual": MANUAL }), MANUAL)
}

fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        linux_status_bytes("VmRSS:")
    }
    #[cfg(windows)]
    {
        windows_memory_bytes().0
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        0
    }
}

fn process_peak_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        linux_status_bytes("VmHWM:")
    }
    #[cfg(windows)]
    {
        windows_memory_bytes().1
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        current_rss_bytes()
    }
}

#[cfg(target_os = "linux")]
fn linux_status_bytes(field: &str) -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                let value = line.strip_prefix(field)?.trim();
                value.split_whitespace().next()?.parse::<u64>().ok()
            })
        })
        .unwrap_or(0)
        .saturating_mul(1024)
}

#[cfg(windows)]
fn windows_memory_bytes() -> (u64, u64) {
    use std::{ffi::c_void, mem::size_of};

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCounters {
        cb: size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: both functions are process-local Win32 queries. `counters` is
    // initialized with the exact C layout and remains valid for the call.
    let succeeded = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            size_of::<ProcessMemoryCounters>() as u32,
        )
    };
    if succeeded == 0 {
        (0, 0)
    } else {
        (
            counters.working_set_size as u64,
            counters.peak_working_set_size as u64,
        )
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn path_envelope_has_role_version_display_and_raw_identity() {
        let envelope = path_envelope(Path::new("music/track.mp3"), "source");
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["role"], "source");
        assert_eq!(envelope["display"], "music/track.mp3");
        assert!(envelope["raw_base64"]
            .as_str()
            .is_some_and(|raw| !raw.is_empty()));
    }

    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_path_identity_is_not_lossy() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = PathBuf::from(OsString::from_vec(vec![0xff]));
        let envelope = path_envelope(&path, "target");
        assert_eq!(envelope["encoding"], "unix_bytes_v1");
        assert_eq!(envelope["raw_base64"], "/w==");
    }

    #[cfg(windows)]
    #[test]
    fn windows_unpaired_utf16_path_identity_is_not_lossy() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        let path = PathBuf::from(OsString::from_wide(&[0xd800]));
        let envelope = path_envelope(&path, "target");
        assert_eq!(envelope["encoding"], "windows_utf16le_v1");
        assert_eq!(envelope["raw_base64"], "ANg=");
    }
}
