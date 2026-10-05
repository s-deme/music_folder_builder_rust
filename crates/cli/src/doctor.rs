use super::*;
use args::DoctorCommand;
use music_folder_core::{
    doctor::{DoctorStore, DoctorUseCase},
    RunStatus,
};
use music_folder_infra::windows_fs::DoctorFileSystem;

pub fn execute(command: DoctorCommand) -> Result<CommandResult, CliFailure> {
    match command {
        DoctorCommand::Scan {
            source,
            db,
            workers,
        } => {
            let store = open_store(&db)?;
            let mut options = scan_options(workers);
            attach_scan_progress(&mut options);
            let run = DoctorUseCase {
                fs: Arc::new(DoctorFileSystem),
                metadata: Arc::new(LoftyMetadataReader),
                store,
            }
            .execute(&source, &options)
            .map_err(CliFailure::from_workflow)?;
            let mut result = CommandResult::success(json!(run), format!(
                "doctor_run_id={} scan_run_id={} status={:?} files={} cache_hits={} failures={} issues={}{}",
                run.id, run.scan_id, run.status, run.files, run.cache_hits, run.failures, run.issue_count,
                run.error.as_ref().map(|e| format!(" error={e}")).unwrap_or_default()));
            (result.status, result.exit_code) = match run.status {
                RunStatus::Completed => ("success", 0),
                RunStatus::Partial => ("partial", output::EXIT_PARTIAL),
                RunStatus::Cancelled => ("cancelled", output::EXIT_CANCELLED),
                _ => ("error", output::EXIT_INTERNAL),
            };
            Ok(result)
        }
        DoctorCommand::Show { run_id, db } => {
            let run = open_store(&db)?
                .doctor_run(&run_id)
                .map_err(|e| CliFailure::from_code(&e))?;
            Ok(CommandResult::success(
                json!(run),
                serde_json::to_string_pretty(&run)
                    .map_err(|e| CliFailure::internal("doctor_json", e.to_string()))?,
            ))
        }
        DoctorCommand::Issues {
            run_id,
            db,
            severity,
            code,
        } => list(&db, &run_id, severity.as_deref(), code.as_deref(), None),
        DoctorCommand::Duplicates { run_id, db } => {
            list(&db, &run_id, None, None, Some("duplicates"))
        }
        DoctorCommand::Albums { run_id, db } => list(&db, &run_id, None, None, Some("albums")),
    }
}

fn list(
    db: &Path,
    id: &str,
    severity: Option<&str>,
    code: Option<&str>,
    category: Option<&str>,
) -> Result<CommandResult, CliFailure> {
    let store = open_store(db)?;
    let run = store
        .doctor_run(id)
        .map_err(|e| CliFailure::from_code(&e))?;
    let issues = store
        .doctor_issues(id, severity, code, category)
        .map_err(|e| CliFailure::from_code(&e))?;
    let ids: HashSet<_> = issues
        .iter()
        .flat_map(|i| i.file_ids.iter().copied())
        .collect();
    let files: Vec<_> = store
        .doctor_files(&run.scan_id)
        .map_err(|e| CliFailure::from_code(&e))?
        .into_iter()
        .filter(|f| ids.contains(&f.id))
        .map(|f| {
            json!({
                "id": f.id, "path": music_folder_infra::path_codec::path_envelope(&f.path, "source"),
                "format": f.path.extension().map(|s| s.to_string_lossy().to_ascii_lowercase()),
                "fingerprint": f.fingerprint, "metadata": f.metadata,
            })
        })
        .collect();
    let data = json!({"doctor_run_id": id, "scan_run_id": run.scan_id, "status": run.status, "issues": issues, "files": files});
    let human = serde_json::to_string_pretty(&data)
        .map_err(|e| CliFailure::internal("doctor_json", e.to_string()))?;
    Ok(CommandResult::success(data, human))
}
