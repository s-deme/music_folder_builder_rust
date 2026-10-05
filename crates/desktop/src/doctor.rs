use super::*;
use music_folder_core::doctor::{DoctorStore, DoctorUseCase, ISSUE_CODES};
use music_folder_infra::{
    artwork::{Artwork, ArtworkService},
    doctor_view::{
        AlbumDetail, AlbumRow, DoctorHistory, DoctorSummary, DoctorView, IssueDetail, IssueRow,
        Page,
    },
    windows_fs::DoctorFileSystem,
};

#[derive(Clone, Serialize)]
pub struct DoctorSnapshot {
    request_id: String,
    status: String,
    doctor_run_id: Option<String>,
    progress: Option<ScanProgress>,
    error: Option<String>,
}
struct DoctorJob {
    snapshot: DoctorSnapshot,
    cancellation: CancellationToken,
}
#[derive(Default)]
struct SelectedView {
    generation: u64,
    run_id: Option<String>,
    view: Option<Arc<DoctorView>>,
}
#[derive(Default, Clone)]
pub struct DoctorState {
    job: Arc<Mutex<Option<DoctorJob>>>,
    selected: Arc<Mutex<SelectedView>>,
    artwork: Arc<ArtworkService>,
}
impl DoctorState {
    fn view(&self, id: &str) -> Result<Arc<DoctorView>, String> {
        validate_opaque_id(id)?;
        let selected = self.selected.lock().map_err(|e| e.to_string())?;
        if selected.run_id.as_deref() != Some(id) {
            return Err("doctor_view_stale".into());
        }
        selected
            .view
            .clone()
            .ok_or_else(|| "doctor_view_not_ready".into())
    }
}

fn reserve_doctor(
    session: &WorkflowSession,
    jobs: &JobRegistry,
    doctor: &DoctorState,
    initial: &DoctorSnapshot,
    token: &CancellationToken,
) -> Result<(), String> {
    // All scan/job starts acquire session before jobs. Diagnosis never replaces
    // organizing IDs, generation, or the current Plan.
    let workflow = session.0.lock().map_err(|e| e.to_string())?;
    let mut activity = jobs.0.lock().map_err(|e| e.to_string())?;
    if workflow.active_scan_request.is_some()
        || workflow.active_job_id.is_some()
        || workflow.active_mutation_job_id.is_some()
        || activity.active_reader_id.is_some()
        || activity.active_mutation_job_id.is_some()
    {
        return Err("workflow_job_busy".into());
    }
    *doctor.job.lock().map_err(|e| e.to_string())? = Some(DoctorJob {
        snapshot: initial.clone(),
        cancellation: token.clone(),
    });
    activity.active_reader_id = Some(initial.request_id.clone());
    Ok(())
}

#[tauri::command]
pub fn start_doctor(
    app: tauri::AppHandle,
    application: tauri::State<'_, ApplicationState>,
    folders: tauri::State<'_, FolderSelectionRegistry>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    doctor: tauri::State<'_, DoctorState>,
    source_selection_id: String,
) -> Result<DoctorSnapshot, String> {
    validate_opaque_id(&source_selection_id)?;
    let source = selected_folder(folders.inner(), &source_selection_id, FolderPurpose::Source)?;
    let id = uuid::Uuid::new_v4().to_string();
    let token = CancellationToken::default();
    let initial = DoctorSnapshot {
        request_id: id.clone(),
        status: "running".into(),
        doctor_run_id: None,
        progress: None,
        error: None,
    };
    reserve_doctor(
        session.inner(),
        jobs.inner(),
        doctor.inner(),
        &initial,
        &token,
    )?;
    let state = doctor.inner().clone();
    let jobs = jobs.inner().clone();
    let store = application.store.clone();
    std::thread::spawn(move || {
        let progress_store = store.clone();
        let progress_state = state.clone();
        let progress_id = id.clone();
        let progress_app = app.clone();
        let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_millis(100)));
        let options = ScanOptions {
            cancellation: token,
            progress: Some(Arc::new(move |p| {
                if let Ok(mut job) = progress_state.job.lock() {
                    if let Some(job) = job
                        .as_mut()
                        .filter(|j| j.snapshot.request_id == progress_id)
                    {
                        if job.snapshot.doctor_run_id.is_none() {
                            job.snapshot.doctor_run_id =
                                progress_store.doctor_id_for_scan(&p.scan_id).ok();
                        }
                        job.snapshot.progress = Some(p);
                        if let Ok(mut last) = last_emit.lock() {
                            if last.elapsed() >= Duration::from_millis(100) {
                                *last = Instant::now();
                                let _ = progress_app.emit("doctor-progress", job.snapshot.clone());
                            }
                        }
                    }
                }
            })),
            ..ScanOptions::default()
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            DoctorUseCase {
                fs: Arc::new(DoctorFileSystem),
                metadata: Arc::new(LoftyMetadataReader),
                store,
            }
            .execute(&source, &options)
        }))
        .map_err(|_| "doctor_job_panicked".to_owned())
        .and_then(|r| r.map_err(|e| e.to_string()));
        let finished = if let Ok(mut job) = state.job.lock() {
            job.as_mut()
                .filter(|j| j.snapshot.request_id == id)
                .map(|j| {
                    match result {
                        Ok(run) => {
                            j.snapshot.status = run.status.as_str().into();
                            j.snapshot.doctor_run_id = Some(run.id);
                            j.snapshot.error = run.error;
                        }
                        Err(e) => {
                            j.snapshot.status = "failed".into();
                            j.snapshot.error = Some(e);
                        }
                    }
                    j.snapshot.clone()
                })
        } else {
            None
        };
        if let Ok(mut activity) = jobs.0.lock() {
            if activity.active_reader_id.as_deref() == Some(&id) {
                activity.active_reader_id = None;
            }
        }
        if let Some(finished) = finished {
            let _ = app.emit("doctor-finished", finished);
        }
    });
    Ok(initial)
}

#[tauri::command]
pub fn doctor_status(
    doctor: tauri::State<'_, DoctorState>,
    request_id: Option<String>,
) -> Result<Option<DoctorSnapshot>, String> {
    if let Some(id) = &request_id {
        validate_opaque_id(id)?;
    }
    let job = doctor.job.lock().map_err(|e| e.to_string())?;
    Ok(job
        .as_ref()
        .filter(|j| {
            request_id
                .as_ref()
                .is_none_or(|id| id == &j.snapshot.request_id)
        })
        .map(|j| j.snapshot.clone()))
}
#[tauri::command]
pub fn cancel_doctor(
    doctor: tauri::State<'_, DoctorState>,
    request_id: String,
) -> Result<(), String> {
    validate_opaque_id(&request_id)?;
    let job = doctor.job.lock().map_err(|e| e.to_string())?;
    let job = job
        .as_ref()
        .filter(|j| j.snapshot.request_id == request_id && j.snapshot.status == "running")
        .ok_or("doctor_job_not_running")?;
    job.cancellation.cancel();
    Ok(())
}
#[tauri::command]
pub async fn doctor_history(
    application: tauri::State<'_, ApplicationState>,
    cursor: Option<usize>,
) -> Result<Page<DoctorHistory>, String> {
    let store = application.store.clone();
    tauri::async_runtime::spawn_blocking(move || store.doctor_history(cursor.unwrap_or(0)))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn open_doctor_view(
    application: tauri::State<'_, ApplicationState>,
    doctor: tauri::State<'_, DoctorState>,
    run_id: String,
) -> Result<DoctorSummary, String> {
    validate_opaque_id(&run_id)?;
    let generation = {
        let mut s = doctor.selected.lock().map_err(|e| e.to_string())?;
        s.generation = s.generation.saturating_add(1);
        s.run_id = Some(run_id.clone());
        s.view = None;
        s.generation
    };
    let store = application.store.clone();
    let state = doctor.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let history = store.doctor_history_entry(&run_id)?;
        let source = store.doctor_source(&run_id)?;
        let files = store.doctor_files(&history.run.scan_id)?;
        let issues = store.doctor_issues(&run_id, None, None, None)?;
        let view = Arc::new(DoctorView::new(history, source, files, issues));
        let summary = view.summary();
        let mut selected = state.selected.lock().map_err(|e| e.to_string())?;
        if selected.generation != generation || selected.run_id.as_deref() != Some(&run_id) {
            return Err("doctor_view_stale".into());
        }
        selected.view = Some(view);
        Ok(summary)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn close_doctor_view(
    doctor: tauri::State<'_, DoctorState>,
    run_id: String,
) -> Result<(), String> {
    validate_opaque_id(&run_id)?;
    let mut selected = doctor.selected.lock().map_err(|e| e.to_string())?;
    if selected.run_id.as_deref() == Some(&run_id) {
        selected.generation = selected.generation.saturating_add(1);
        selected.run_id = None;
        selected.view = None;
    }
    Ok(())
}
#[tauri::command]
pub async fn doctor_issue_page(
    application: tauri::State<'_, ApplicationState>,
    run_id: String,
    cursor: Option<usize>,
    severity: Option<String>,
    code: Option<String>,
    category: Option<String>,
) -> Result<Page<IssueRow>, String> {
    validate_opaque_id(&run_id)?;
    if severity
        .as_ref()
        .is_some_and(|s| !["critical", "warning", "info"].contains(&s.as_str()))
        || code
            .as_ref()
            .is_some_and(|c| !ISSUE_CODES.contains(&c.as_str()))
        || category.as_ref().is_some_and(|c| {
            !["read", "tags", "variants", "duplicates", "albums"].contains(&c.as_str())
        })
    {
        return Err("doctor_filter_invalid".into());
    }
    let store = application.store.clone();
    tauri::async_runtime::spawn_blocking(move || {
        store.doctor_issue_page(
            &run_id,
            cursor.unwrap_or(0),
            severity.as_deref(),
            code.as_deref(),
            category.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn doctor_issue_detail(
    doctor: tauri::State<'_, DoctorState>,
    run_id: String,
    ordinal: usize,
    cursor: Option<usize>,
) -> Result<IssueDetail, String> {
    let view = doctor.view(&run_id)?;
    tauri::async_runtime::spawn_blocking(move || view.issue_detail(ordinal, cursor.unwrap_or(0)))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn doctor_album_page(
    doctor: tauri::State<'_, DoctorState>,
    run_id: String,
    query: Option<String>,
    cursor: Option<usize>,
) -> Result<Page<AlbumRow>, String> {
    validate_optional_query(query.as_deref())?;
    let view = doctor.view(&run_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        Ok(view.album_page(query.as_deref().unwrap_or(""), cursor.unwrap_or(0)))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn doctor_album_detail(
    doctor: tauri::State<'_, DoctorState>,
    run_id: String,
    album_id: String,
    cursor: Option<usize>,
    issue_cursor: Option<usize>,
) -> Result<AlbumDetail, String> {
    validate_opaque_id(&album_id)?;
    let view = doctor.view(&run_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        view.album_detail(&album_id, cursor.unwrap_or(0), issue_cursor.unwrap_or(0))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn doctor_artwork(
    doctor: tauri::State<'_, DoctorState>,
    run_id: String,
    album_id: String,
) -> Result<Artwork, String> {
    validate_opaque_id(&album_id)?;
    let view = doctor.view(&run_id)?;
    let service = doctor.artwork.clone();
    let state = doctor.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        Ok(service.load_current(&view, view.album(&album_id)?, || {
            state
                .view(&run_id)
                .is_ok_and(|selected| Arc::ptr_eq(&selected, &view))
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn doctor_source_selection(
    application: tauri::State<'_, ApplicationState>,
    folders: tauri::State<'_, FolderSelectionRegistry>,
    run_id: String,
) -> Result<FolderSelectionResponse, String> {
    validate_opaque_id(&run_id)?;
    let path = application.store.doctor_source(&run_id)?;
    let selection_id = uuid::Uuid::new_v4().to_string();
    let display = path.to_string_lossy().into_owned();
    folders.0.lock().map_err(|e| e.to_string())?.insert(
        selection_id.clone(),
        FolderSelection {
            path,
            purpose: FolderPurpose::Source,
        },
    );
    Ok(FolderSelectionResponse {
        selection_id,
        display,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn doctor_reservation_preserves_plan_and_excludes_every_other_job() {
        let session = WorkflowSession::default();
        let jobs = JobRegistry::default();
        let doctor = DoctorState::default();
        session.0.lock().unwrap().plan_id = Some("plan-1".into());
        let initial = DoctorSnapshot {
            request_id: "doctor-1".into(),
            status: "running".into(),
            doctor_run_id: None,
            progress: None,
            error: None,
        };
        reserve_doctor(
            &session,
            &jobs,
            &doctor,
            &initial,
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(session.0.lock().unwrap().plan_id.as_deref(), Some("plan-1"));
        assert_eq!(session.0.lock().unwrap().workflow_generation, 0);
        assert!(reserve_doctor(
            &session,
            &jobs,
            &doctor,
            &initial,
            &CancellationToken::default()
        )
        .is_err());
        assert_eq!(
            begin_job(&session, &jobs, 0, JobKind::Apply, false, "plan-1").unwrap_err(),
            "mutation_job_busy"
        );
        assert!(reserve_external_mutation(&jobs).is_err());
        assert!(ensure_idle_workflow_context(&session.0.lock().unwrap(), &jobs, 0).is_err());
        jobs.0.lock().unwrap().active_reader_id = None;
        let job = begin_job(&session, &jobs, 0, JobKind::Apply, false, "plan-1").unwrap();
        assert!(reserve_doctor(
            &session,
            &jobs,
            &doctor,
            &initial,
            &CancellationToken::default()
        )
        .is_err());
        let result = Ok(WorkflowResponse {
            id: "apply-1".into(),
            success: 0,
            skipped: 0,
            failed: 0,
        });
        finish_job_session(&session, &job, &result);
        jobs.finish(&job.job_id, result).unwrap();
        session.0.lock().unwrap().active_scan_request = Some("scan-1".into());
        assert!(reserve_doctor(
            &session,
            &jobs,
            &doctor,
            &initial,
            &CancellationToken::default()
        )
        .is_err());
    }
    #[test]
    fn doctor_views_reject_unselected_runs_and_raw_paths() {
        let doctor = DoctorState::default();
        assert!(doctor.view("C:\\Music\\cover.png").is_err());
        assert!(doctor.view("unselected-run").is_err());
        doctor.selected.lock().unwrap().run_id = Some("selected-run".into());
        assert!(doctor.view("selected-run").is_err());
    }
}
