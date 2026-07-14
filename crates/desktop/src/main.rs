use music_folder_core::usecases::{
    ApplyUseCase, PlanOptions, PlanUseCase, RollbackUseCase, ScanOptions, ScanUseCase,
    VerifyUseCase,
};
use music_folder_core::usecases::{CancellationToken, ScanProgress};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::LocalFileSystem,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::Emitter;

#[derive(Default, Clone)]
struct ScanRegistry(Arc<Mutex<HashMap<String, ScanState>>>);

#[derive(Clone)]
struct ScanState {
    token: CancellationToken,
    status: ScanStatus,
}

#[derive(Clone, Serialize)]
struct ScanStatus {
    request_id: String,
    status: String,
    scan_id: Option<String>,
    files: u64,
    cache_hits: u64,
    warnings: u64,
    error: Option<String>,
}

#[derive(Serialize)]
struct ScanResponse {
    scan_id: String,
    files: u64,
    cache_hits: u64,
    warnings: u64,
}
#[derive(Serialize)]
struct WorkflowResponse {
    id: String,
    success: u64,
    skipped: u64,
    failed: u64,
}
#[tauri::command]
fn scan_library(
    app: tauri::AppHandle,
    registry: tauri::State<'_, ScanRegistry>,
    source: String,
    database: String,
    workers: Option<usize>,
) -> Result<ScanResponse, String> {
    let mut options = ScanOptions::default();
    if let Some(value) = workers {
        options.workers = value.max(1);
    }
    let token = CancellationToken::default();
    let registry_value = registry.inner().clone();
    let token_for_progress = token.clone();
    options.cancellation = token;
    options.progress = Some(Arc::new(move |progress: ScanProgress| {
        if let Ok(mut scans) = registry_value.0.lock() {
            scans
                .entry(progress.scan_id.clone())
                .or_insert_with(|| ScanState {
                    token: token_for_progress.clone(),
                    status: ScanStatus {
                        request_id: progress.scan_id.clone(),
                        status: "running".into(),
                        scan_id: Some(progress.scan_id.clone()),
                        files: progress.processed,
                        cache_hits: progress.cache_hits,
                        warnings: progress.warnings,
                        error: None,
                    },
                });
        }
        let _ = app.emit("scan-progress", progress);
    }));
    let usecase = ScanUseCase {
        fs: Arc::new(LocalFileSystem),
        metadata: Arc::new(LoftyMetadataReader),
        store: Arc::new(SqliteScanStore::open(&PathBuf::from(database))?),
    };
    let result = usecase.execute(&PathBuf::from(source), &options)?;
    if let Ok(mut scans) = registry.0.lock() {
        scans.remove(&result.scan_id);
    }
    Ok(ScanResponse {
        scan_id: result.scan_id,
        files: result.files,
        cache_hits: result.cache_hits,
        warnings: result.warnings,
    })
}

#[tauri::command]
fn start_scan(
    app: tauri::AppHandle,
    registry: tauri::State<'_, ScanRegistry>,
    source: String,
    database: String,
    workers: Option<usize>,
) -> Result<ScanStatus, String> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let token = CancellationToken::default();
    let initial = ScanStatus {
        request_id: request_id.clone(),
        status: "running".into(),
        scan_id: None,
        files: 0,
        cache_hits: 0,
        warnings: 0,
        error: None,
    };
    registry
        .0
        .lock()
        .map_err(|_| "scan registry poisoned".to_string())?
        .insert(
            request_id.clone(),
            ScanState {
                token: token.clone(),
                status: initial.clone(),
            },
        );
    let registry = registry.inner().clone();
    let request_for_thread = request_id.clone();
    std::thread::spawn(move || {
        let mut options = ScanOptions::default();
        if let Some(value) = workers {
            options.workers = value.max(1);
        }
        options.cancellation = token;
        let registry_for_progress = registry.clone();
        let request_for_progress = request_for_thread.clone();
        let app_for_progress = app.clone();
        let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_millis(100)));
        options.progress = Some(Arc::new(move |progress: ScanProgress| {
            if let Ok(mut scans) = registry_for_progress.0.lock() {
                if let Some(entry) = scans.get_mut(&request_for_progress) {
                    entry.status.scan_id = Some(progress.scan_id.clone());
                    entry.status.files = progress.processed;
                    entry.status.cache_hits = progress.cache_hits;
                    entry.status.warnings = progress.warnings;
                }
            }
            if let Ok(mut last) = last_emit.lock() {
                if last.elapsed() >= Duration::from_millis(100) {
                    *last = Instant::now();
                    let _ = app_for_progress.emit("scan-progress", progress);
                }
            }
        }));
        let result = SqliteScanStore::open(&PathBuf::from(database)).and_then(|store| {
            ScanUseCase {
                fs: Arc::new(LocalFileSystem),
                metadata: Arc::new(LoftyMetadataReader),
                store: Arc::new(store),
            }
            .execute(&PathBuf::from(source), &options)
        });
        let mut status = match result {
            Ok(result) => ScanStatus {
                request_id: request_for_thread.clone(),
                status: if options.cancellation.is_cancelled() {
                    "cancelled"
                } else {
                    "completed"
                }
                .into(),
                scan_id: Some(result.scan_id),
                files: result.files,
                cache_hits: result.cache_hits,
                warnings: result.warnings,
                error: None,
            },
            Err(error) => ScanStatus {
                request_id: request_for_thread.clone(),
                status: "failed".into(),
                scan_id: None,
                files: 0,
                cache_hits: 0,
                warnings: 0,
                error: Some(error),
            },
        };
        if let Ok(mut scans) = registry.0.lock() {
            if let Some(entry) = scans.get_mut(&request_for_thread) {
                status.scan_id = status.scan_id.or_else(|| entry.status.scan_id.clone());
                entry.status = status.clone();
            }
        }
        let _ = app.emit("scan-finished", status);
    });
    Ok(initial)
}

#[tauri::command]
fn scan_status(
    request_id: String,
    registry: tauri::State<'_, ScanRegistry>,
) -> Result<ScanStatus, String> {
    let mut scans = registry
        .0
        .lock()
        .map_err(|_| "scan registry poisoned".to_string())?;
    let status = scans
        .get(&request_id)
        .map(|entry| entry.status.clone())
        .ok_or_else(|| "scan_not_found".to_string())?;
    if status.status != "running" {
        scans.remove(&request_id);
    }
    Ok(status)
}
#[tauri::command]
fn cancel_scan(scan_id: String, registry: tauri::State<'_, ScanRegistry>) -> Result<(), String> {
    let token = registry
        .0
        .lock()
        .map_err(|_| "scan registry poisoned".to_string())?
        .get(&scan_id)
        .map(|entry| entry.token.clone())
        .ok_or_else(|| "scan_not_running".to_string())?;
    token.cancel();
    Ok(())
}
fn store(database: &str) -> Result<Arc<SqliteScanStore>, String> {
    Ok(Arc::new(SqliteScanStore::open(&PathBuf::from(database))?))
}
#[tauri::command]
fn create_plan(
    scan_id: String,
    target: String,
    database: String,
) -> Result<WorkflowResponse, String> {
    let result = PlanUseCase {
        store: store(&database)?,
    }
    .execute(
        &scan_id,
        &PlanOptions {
            target_root: PathBuf::from(target),
            batch_size: 250,
        },
    )?;
    Ok(WorkflowResponse {
        id: result.plan_id,
        success: result.items,
        skipped: result.conflicts,
        failed: result.risks,
    })
}
#[tauri::command]
fn apply_plan(
    plan_id: String,
    database: String,
    execute: bool,
) -> Result<WorkflowResponse, String> {
    let result = ApplyUseCase {
        store: store(&database)?,
        files: Arc::new(LocalFileSystem),
    }
    .execute(&plan_id, !execute)?;
    Ok(WorkflowResponse {
        id: result.execution_id,
        success: result.success,
        skipped: result.skipped,
        failed: result.failed,
    })
}
#[tauri::command]
fn verify_execution(execution_id: String, database: String) -> Result<WorkflowResponse, String> {
    let result = VerifyUseCase {
        store: store(&database)?,
        files: Arc::new(LocalFileSystem),
    }
    .execute(&execution_id)?;
    Ok(WorkflowResponse {
        id: result.verify_id,
        success: result.success,
        skipped: 0,
        failed: result.failed,
    })
}
#[tauri::command]
fn rollback_execution(
    execution_id: String,
    database: String,
    execute: bool,
) -> Result<WorkflowResponse, String> {
    let result = RollbackUseCase {
        store: store(&database)?,
        files: Arc::new(LocalFileSystem),
    }
    .execute(&execution_id, !execute)?;
    Ok(WorkflowResponse {
        id: result.rollback_id,
        success: result.success,
        skipped: result.skipped,
        failed: result.failed,
    })
}
#[tauri::command]
fn list_history(
    database: String,
    limit: u32,
    cursor: Option<i64>,
) -> Result<Vec<music_folder_infra::sqlite::HistoryRow>, String> {
    store(&database)?.list_history(limit.min(200), cursor)
}
#[tauri::command]
fn get_run_detail(
    database: String,
    kind: String,
    run_id: String,
) -> Result<music_folder_infra::sqlite::RunDetailRow, String> {
    store(&database)?.get_run_detail(&kind, &run_id)
}
#[tauri::command]
fn list_plan_items(
    database: String,
    plan_id: String,
    cursor: Option<u64>,
    limit: u32,
    query: Option<String>,
    risk: Option<String>,
) -> Result<Vec<music_folder_infra::sqlite::PlanItemRow>, String> {
    store(&database)?.list_plan_items(&plan_id, cursor, limit, query.as_deref(), risk.as_deref())
}
#[tauri::command]
fn list_operation_logs(
    database: String,
    execution_id: String,
    cursor: Option<u64>,
    limit: u32,
    query: Option<String>,
    result: Option<String>,
) -> Result<Vec<music_folder_infra::sqlite::OperationLogRow>, String> {
    store(&database)?.list_operation_logs(
        &execution_id,
        cursor,
        limit,
        query.as_deref(),
        result.as_deref(),
    )
}
#[tauri::command]
fn list_metrics(
    database: String,
    run_id: String,
) -> Result<Vec<music_folder_infra::sqlite::MetricRow>, String> {
    store(&database)?.list_metrics(&run_id)
}
fn main() {
    tauri::Builder::default()
        .manage(ScanRegistry::default())
        .invoke_handler(tauri::generate_handler![
            scan_library,
            start_scan,
            scan_status,
            cancel_scan,
            create_plan,
            apply_plan,
            verify_execution,
            rollback_execution,
            list_history,
            get_run_detail,
            list_plan_items,
            list_operation_logs,
            list_metrics
        ])
        .run(tauri::generate_context!())
        .expect("failed to run desktop application");
}
