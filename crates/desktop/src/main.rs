use music_folder_core::ports::{ManualTargetChange, RecoveryStore};
use music_folder_core::usecases::{
    ApplyUseCase, PlanOptions, PlanUseCase, RecoveryUseCase, RollbackUseCase, ScanOptions,
    ScanUseCase, VerifyUseCase,
};
use music_folder_core::usecases::{CancellationToken, ScanProgress};
use music_folder_core::{RecoveryAction, VerifySubject};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::LocalFileSystem,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

struct ApplicationState {
    store: Arc<SqliteScanStore>,
}

#[derive(Default, Clone)]
struct WorkflowSession(Arc<Mutex<WorkflowSessionState>>);

#[derive(Clone, Default)]
struct WorkflowSessionState {
    workflow_generation: u64,
    active_scan_request: Option<String>,
    active_job_id: Option<String>,
    latest_job_id: Option<String>,
    active_mutation_job_id: Option<String>,
    scan_id: Option<String>,
    plan_id: Option<String>,
    execution_id: Option<String>,
    view_only: bool,
    recovery_operation_ids: HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JobKind {
    Plan,
    PlanRevision,
    Apply,
    Verify,
    Rollback,
}

impl JobKind {
    const fn phase(self, execute: bool) -> &'static str {
        match self {
            Self::Plan | Self::PlanRevision => "planning",
            Self::Apply if execute => "applying",
            Self::Apply => "preflighting",
            Self::Verify => "verifying",
            Self::Rollback if execute => "rolling_back",
            Self::Rollback => "preflighting_rollback",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct JobSnapshot {
    job_id: String,
    workflow_generation: u64,
    event_seq: u64,
    kind: JobKind,
    phase: String,
    status: String,
    input_id: String,
    run_id: Option<String>,
    scan_id: Option<String>,
    plan_id: Option<String>,
    execution_id: Option<String>,
    result: Option<WorkflowResponse>,
    error: Option<String>,
}

impl JobSnapshot {
    fn running(
        job_id: String,
        workflow_generation: u64,
        kind: JobKind,
        execute: bool,
        input_id: String,
        workflow: &WorkflowSessionState,
    ) -> Self {
        Self {
            job_id,
            workflow_generation,
            event_seq: 1,
            kind,
            phase: kind.phase(execute).into(),
            status: "running".into(),
            input_id,
            run_id: None,
            scan_id: workflow.scan_id.clone(),
            plan_id: workflow.plan_id.clone(),
            execution_id: workflow.execution_id.clone(),
            result: None,
            error: None,
        }
    }
}

#[derive(Default)]
struct JobRegistryState {
    jobs: HashMap<String, JobSnapshot>,
    active_mutation_job_id: Option<String>,
}

#[derive(Default, Clone)]
struct JobRegistry(Arc<Mutex<JobRegistryState>>);

impl JobRegistry {
    fn snapshot(
        &self,
        job_id: &str,
        after_seq: Option<u64>,
    ) -> Result<Option<JobSnapshot>, String> {
        let jobs = self
            .0
            .lock()
            .map_err(|_| "job registry poisoned".to_string())?;
        let job = jobs
            .jobs
            .get(job_id)
            .ok_or_else(|| "workflow_job_not_found".to_string())?;
        Ok((after_seq.is_none_or(|sequence| job.event_seq > sequence)).then(|| job.clone()))
    }

    fn has_active_mutation(&self) -> Result<bool, String> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "job registry poisoned".to_string())?
            .active_mutation_job_id
            .is_some())
    }

    fn supersede(&self, job_id: &str) -> Result<(), String> {
        let mut jobs = self
            .0
            .lock()
            .map_err(|_| "job registry poisoned".to_string())?;
        let Some(job) = jobs.jobs.get_mut(job_id) else {
            return Ok(());
        };
        if job.status == "running" {
            job.event_seq = job.event_seq.saturating_add(1);
            job.status = "cancelled".into();
            job.error = Some("workflow_context_superseded".into());
        }
        Ok(())
    }

    fn finish(&self, job_id: &str, result: Result<WorkflowResponse, String>) -> Result<(), String> {
        let mut jobs = self
            .0
            .lock()
            .map_err(|_| "job registry poisoned".to_string())?;
        let job = jobs
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| "workflow_job_not_found".to_string())?;
        if job.status != "running" {
            return Ok(());
        }
        job.event_seq = job.event_seq.saturating_add(1);
        match result {
            Ok(value) => {
                job.status = "completed".into();
                job.run_id = Some(value.id.clone());
                job.result = Some(value);
            }
            Err(error) => {
                job.status = "failed".into();
                job.error = Some(error);
            }
        }
        if jobs.active_mutation_job_id.as_deref() == Some(job_id) {
            jobs.active_mutation_job_id = None;
        }
        Ok(())
    }
}

struct ExternalMutationGuard {
    registry: JobRegistry,
    id: String,
}

impl Drop for ExternalMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut jobs) = self.registry.0.lock() {
            if jobs.active_mutation_job_id.as_deref() == Some(&self.id) {
                jobs.active_mutation_job_id = None;
            }
        }
    }
}

fn reserve_external_mutation(registry: &JobRegistry) -> Result<ExternalMutationGuard, String> {
    let id = format!("external-{}", uuid::Uuid::new_v4());
    let mut jobs = registry
        .0
        .lock()
        .map_err(|_| "job registry poisoned".to_string())?;
    if jobs.active_mutation_job_id.is_some() {
        return Err("mutation_job_busy".into());
    }
    jobs.active_mutation_job_id = Some(id.clone());
    drop(jobs);
    Ok(ExternalMutationGuard {
        registry: registry.clone(),
        id,
    })
}

impl WorkflowSession {
    fn can_view_plan(&self, plan_id: &str) -> Result<(), String> {
        let state = self
            .0
            .lock()
            .map_err(|_| "workflow session poisoned".to_string())?;
        if state.plan_id.as_deref() != Some(plan_id) {
            return Err("workflow_context_stale".into());
        }
        Ok(())
    }

    fn can_view_execution(&self, execution_id: &str) -> Result<(), String> {
        let state = self
            .0
            .lock()
            .map_err(|_| "workflow session poisoned".to_string())?;
        if state.execution_id.as_deref() != Some(execution_id) {
            return Err("workflow_context_stale".into());
        }
        Ok(())
    }
}

#[derive(Default, Clone)]
struct FolderSelectionRegistry(Arc<Mutex<HashMap<String, FolderSelection>>>);

#[derive(Clone)]
struct FolderSelection {
    path: PathBuf,
    purpose: FolderPurpose,
}

#[derive(Serialize)]
struct FolderSelectionResponse {
    selection_id: String,
    display: String,
}

#[derive(Default, Clone)]
struct PlanTargetAuthorizationRegistry(Arc<Mutex<HashMap<String, AuthorizedPlanTarget>>>);

#[derive(Clone)]
struct AuthorizedPlanTarget {
    plan_id: String,
    plan_item_id: String,
    workflow_generation: u64,
    target: PathBuf,
    issued_at: Instant,
}

#[derive(Serialize)]
struct PlanTargetAuthorizationResponse {
    target_capability_id: String,
}

impl PlanTargetAuthorizationRegistry {
    fn issue(
        &self,
        plan_id: String,
        plan_item_id: String,
        workflow_generation: u64,
        target: PathBuf,
    ) -> Result<PlanTargetAuthorizationResponse, String> {
        let capability_id = uuid::Uuid::new_v4().to_string();
        let mut authorizations = self
            .0
            .lock()
            .map_err(|_| "plan target authorization registry poisoned".to_string())?;
        if authorizations.len() >= 256 {
            if let Some(oldest) = authorizations
                .iter()
                .min_by_key(|(_, authorization)| authorization.issued_at)
                .map(|(id, _)| id.clone())
            {
                authorizations.remove(&oldest);
            }
        }
        authorizations.insert(
            capability_id.clone(),
            AuthorizedPlanTarget {
                plan_id,
                plan_item_id,
                workflow_generation,
                target,
                issued_at: Instant::now(),
            },
        );
        Ok(PlanTargetAuthorizationResponse {
            target_capability_id: capability_id,
        })
    }

    fn consume(
        &self,
        capability_id: &str,
        plan_id: &str,
        plan_item_id: &str,
        workflow_generation: u64,
    ) -> Result<PathBuf, String> {
        let authorization = self
            .0
            .lock()
            .map_err(|_| "plan target authorization registry poisoned".to_string())?
            .remove(capability_id)
            .ok_or_else(|| "plan_target_capability_not_found".to_string())?;
        if authorization.plan_id != plan_id
            || authorization.plan_item_id != plan_item_id
            || authorization.workflow_generation != workflow_generation
            || authorization.issued_at.elapsed() > Duration::from_secs(10 * 60)
        {
            return Err("plan_target_capability_mismatch".into());
        }
        Ok(authorization.target)
    }
}

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
    workflow_generation: u64,
    status: String,
    scan_id: Option<String>,
    files: u64,
    cache_hits: u64,
    warnings: u64,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct ScanProgressEvent {
    request_id: String,
    workflow_generation: u64,
    #[serde(flatten)]
    progress: ScanProgress,
}

#[derive(Clone, Debug, Serialize)]
struct WorkflowResponse {
    id: String,
    success: u64,
    skipped: u64,
    failed: u64,
}

#[derive(Clone, Debug, Serialize)]
struct WorkflowActionCapability {
    allowed: bool,
    protection_reasons: Vec<String>,
    confirmation_subject: Option<String>,
}

impl WorkflowActionCapability {
    fn from_reasons(protection_reasons: Vec<String>, confirmation_subject: Option<String>) -> Self {
        Self {
            allowed: protection_reasons.is_empty(),
            protection_reasons,
            confirmation_subject,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct WorkflowCapabilitiesResponse {
    workflow_generation: u64,
    scan_id: Option<String>,
    plan_id: Option<String>,
    execution_id: Option<String>,
    recovery_count: u64,
    apply_dry_run: WorkflowActionCapability,
    apply_execute: WorkflowActionCapability,
    verify: WorkflowActionCapability,
    rollback_dry_run: WorkflowActionCapability,
    rollback_execute: WorkflowActionCapability,
    recovery_execute: WorkflowActionCapability,
}

const MAX_COMMAND_ID_BYTES: usize = 128;
const MAX_QUERY_BYTES: usize = 512;

fn validate_opaque_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_COMMAND_ID_BYTES
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    {
        return Err("invalid_command_id".into());
    }
    Ok(())
}

fn validate_optional_query(value: Option<&str>) -> Result<(), String> {
    if value.is_some_and(|query| query.len() > MAX_QUERY_BYTES || query.contains('\0')) {
        return Err("command_query_too_large".into());
    }
    Ok(())
}

fn validate_history_kind(kind: &str) -> Result<(), String> {
    if matches!(
        kind,
        "scan" | "plan" | "apply" | "verify" | "rollback" | "recovery" | "archive"
    ) {
        Ok(())
    } else {
        Err("invalid_run_kind".into())
    }
}

fn require_execute_confirmation(
    execute: bool,
    confirmation: Option<&str>,
    expected_subject: &str,
) -> Result<(), String> {
    if let Some(value) = confirmation {
        validate_opaque_id(value)?;
    }
    if execute && confirmation != Some(expected_subject) {
        return Err("operation_confirmation_required".into());
    }
    Ok(())
}

fn ensure_idle_workflow_context(
    workflow: &WorkflowSessionState,
    jobs: &JobRegistry,
    expected_generation: u64,
) -> Result<(), String> {
    if workflow.workflow_generation != expected_generation {
        return Err("workflow_generation_stale".into());
    }
    if workflow.active_scan_request.is_some() {
        return Err("scan_job_busy".into());
    }
    if workflow.active_mutation_job_id.is_some() || jobs.has_active_mutation()? {
        return Err("mutation_job_busy".into());
    }
    if workflow.active_job_id.is_some() {
        return Err("workflow_job_busy".into());
    }
    Ok(())
}

fn begin_job(
    session: &WorkflowSession,
    registry: &JobRegistry,
    expected_generation: u64,
    kind: JobKind,
    execute: bool,
    input_id: &str,
) -> Result<JobSnapshot, String> {
    let mutation = execute && matches!(kind, JobKind::Apply | JobKind::Rollback);
    let mut workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    if workflow.workflow_generation != expected_generation {
        return Err("workflow_generation_stale".into());
    }
    if workflow.view_only {
        return Err("workflow_context_stale".into());
    }
    if workflow.active_scan_request.is_some() {
        return Err("scan_job_busy".into());
    }
    match kind {
        JobKind::Plan => {
            if workflow.scan_id.as_deref() != Some(input_id) {
                return Err("workflow_context_stale".into());
            }
        }
        JobKind::PlanRevision | JobKind::Apply => {
            if workflow.plan_id.as_deref() != Some(input_id) {
                return Err("workflow_context_stale".into());
            }
        }
        JobKind::Verify | JobKind::Rollback => {
            if workflow.execution_id.as_deref() != Some(input_id) {
                return Err("workflow_context_stale".into());
            }
        }
    }

    let mut jobs = registry
        .0
        .lock()
        .map_err(|_| "job registry poisoned".to_string())?;
    if jobs.active_mutation_job_id.is_some() {
        return Err("mutation_job_busy".into());
    }
    if workflow.active_job_id.is_some() {
        return Err("workflow_job_busy".into());
    }

    let generation = workflow.workflow_generation.saturating_add(1);
    let job_id = uuid::Uuid::new_v4().to_string();
    let snapshot = JobSnapshot::running(
        job_id.clone(),
        generation,
        kind,
        execute,
        input_id.to_owned(),
        &workflow,
    );
    workflow.workflow_generation = generation;
    workflow.active_job_id = Some(job_id.clone());
    workflow.latest_job_id = Some(job_id.clone());
    if mutation {
        workflow.active_mutation_job_id = Some(job_id.clone());
        jobs.active_mutation_job_id = Some(job_id.clone());
    }
    if jobs.jobs.len() >= 256 {
        let mut terminal = jobs
            .jobs
            .values()
            .filter(|job| job.status != "running")
            .map(|job| (job.workflow_generation, job.job_id.clone()))
            .collect::<Vec<_>>();
        terminal.sort();
        for (_, id) in terminal.into_iter().take(64) {
            jobs.jobs.remove(&id);
        }
    }
    jobs.jobs.insert(job_id, snapshot.clone());
    Ok(snapshot)
}

fn finish_job_session(
    session: &WorkflowSession,
    job: &JobSnapshot,
    result: &Result<WorkflowResponse, String>,
) {
    let Ok(mut workflow) = session.0.lock() else {
        return;
    };
    if workflow.active_mutation_job_id.as_deref() == Some(&job.job_id) {
        workflow.active_mutation_job_id = None;
    }
    if workflow.workflow_generation != job.workflow_generation
        || workflow.active_job_id.as_deref() != Some(&job.job_id)
    {
        return;
    }
    workflow.active_job_id = None;
    let Ok(value) = result else {
        return;
    };
    match job.kind {
        JobKind::Plan | JobKind::PlanRevision => {
            workflow.plan_id = Some(value.id.clone());
            workflow.execution_id = None;
        }
        JobKind::Apply => workflow.execution_id = Some(value.id.clone()),
        JobKind::Verify | JobKind::Rollback => {}
    }
}

fn run_job<F>(session: WorkflowSession, registry: JobRegistry, job: JobSnapshot, work: F)
where
    F: FnOnce() -> Result<WorkflowResponse, String> + Send + 'static,
{
    std::thread::spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(work))
            .map_err(|_| "workflow_job_panicked".to_string())
            .and_then(|value| value);
        finish_job_session(&session, &job, &result);
        let _ = registry.finish(&job.job_id, result);
    });
}

#[derive(Clone, Serialize)]
struct RecoveryItemResponse {
    operation_id: String,
    attempt_id: String,
    sequence_no: u64,
    state: String,
    source: music_folder_infra::path_codec::LosslessPathEnvelope,
    target: music_folder_infra::path_codec::LosslessPathEnvelope,
    temporary: music_folder_infra::path_codec::LosslessPathEnvelope,
    expected_size: Option<u64>,
    suggested_action: String,
}

#[derive(Serialize)]
struct RecoveryWorkflowResponse {
    recovery_id: String,
    operation_id: String,
    action: String,
    dry_run: bool,
    verify_id: Option<String>,
    verify_success: u64,
    verify_failed: u64,
    workflow_generation: u64,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FolderPurpose {
    Source,
    Target,
}

impl FolderPurpose {
    const fn title(self) -> &'static str {
        match self {
            Self::Source => "音楽ライブラリのフォルダを選択",
            Self::Target => "整理後の保存先フォルダを選択",
        }
    }
}

#[tauri::command]
fn desktop_backend_ready(_application: tauri::State<'_, ApplicationState>) -> bool {
    true
}

fn workflow_action_reasons(
    workflow: &WorkflowSessionState,
    registry_mutation_active: bool,
    required_subject: Option<&str>,
    pending_recovery: bool,
    block_view_only: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let mut add = |condition: bool, code: &str| {
        if condition && !reasons.iter().any(|value| value == code) {
            reasons.push(code.to_owned());
        }
    };
    add(
        block_view_only && workflow.view_only,
        "workflow_archived_read_only",
    );
    add(workflow.active_scan_request.is_some(), "scan_job_busy");
    add(workflow.active_job_id.is_some(), "workflow_job_busy");
    add(
        workflow.active_mutation_job_id.is_some() || registry_mutation_active,
        "mutation_job_busy",
    );
    add(required_subject.is_none(), "workflow_subject_missing");
    add(pending_recovery, "recovery_required");
    reasons
}

#[tauri::command]
fn get_workflow_capabilities(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
) -> Result<WorkflowCapabilitiesResponse, String> {
    // This response is advisory state for rendering. Every command still
    // repeats the generation, subject, recovery and lease checks at commit.
    let recovery_count =
        u64::try_from(application.store.list_recovery_items()?.len()).unwrap_or(u64::MAX);
    let pending_recovery = recovery_count > 0;
    let workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?
        .clone();
    let registry_mutation_active = jobs.has_active_mutation()?;

    let plan_subject = workflow.plan_id.as_deref();
    let execution_subject = workflow.execution_id.as_deref();
    let apply_reasons = workflow_action_reasons(
        &workflow,
        registry_mutation_active,
        plan_subject,
        pending_recovery,
        true,
    );
    let execution_reasons = workflow_action_reasons(
        &workflow,
        registry_mutation_active,
        execution_subject,
        pending_recovery,
        true,
    );
    let mut recovery_reasons = workflow_action_reasons(
        &workflow,
        registry_mutation_active,
        Some("recovery"),
        false,
        false,
    );
    if !pending_recovery {
        recovery_reasons.push("recovery_not_required".into());
    }

    Ok(WorkflowCapabilitiesResponse {
        workflow_generation: workflow.workflow_generation,
        scan_id: workflow.scan_id,
        plan_id: workflow.plan_id.clone(),
        execution_id: workflow.execution_id.clone(),
        recovery_count,
        apply_dry_run: WorkflowActionCapability::from_reasons(apply_reasons.clone(), None),
        apply_execute: WorkflowActionCapability::from_reasons(apply_reasons, workflow.plan_id),
        verify: WorkflowActionCapability::from_reasons(execution_reasons.clone(), None),
        rollback_dry_run: WorkflowActionCapability::from_reasons(execution_reasons.clone(), None),
        rollback_execute: WorkflowActionCapability::from_reasons(
            execution_reasons,
            workflow.execution_id,
        ),
        recovery_execute: WorkflowActionCapability::from_reasons(recovery_reasons, None),
    })
}

#[tauri::command]
async fn pick_folder(
    app: tauri::AppHandle,
    registry: tauri::State<'_, FolderSelectionRegistry>,
    purpose: FolderPurpose,
    current_selection_id: Option<String>,
) -> Result<Option<FolderSelectionResponse>, String> {
    let mut picker = app.dialog().file().set_title(purpose.title());
    if let Some(current) = current_selection_id {
        let selected = registry
            .0
            .lock()
            .map_err(|_| "folder selection registry poisoned".to_string())?
            .get(&current)
            .filter(|selection| selection.purpose == purpose)
            .cloned()
            .ok_or_else(|| "folder_selection_not_found".to_string())?;
        picker = picker.set_directory(selected.path);
    }

    let Some(selected) = picker.blocking_pick_folder() else {
        return Ok(None);
    };
    let selected = selected
        .into_path()
        .map_err(|error| format!("選択したフォルダをパスへ変換できません: {error}"))?;
    if !selected.is_absolute() {
        return Err("選択したフォルダが絶対パスではありません".into());
    }
    let selection_id = uuid::Uuid::new_v4().to_string();
    let display = selected.to_string_lossy().into_owned();
    registry
        .0
        .lock()
        .map_err(|_| "folder selection registry poisoned".to_string())?
        .insert(
            selection_id.clone(),
            FolderSelection {
                path: selected,
                purpose,
            },
        );
    Ok(Some(FolderSelectionResponse {
        selection_id,
        display,
    }))
}

fn selected_folder(
    registry: &FolderSelectionRegistry,
    selection_id: &str,
    purpose: FolderPurpose,
) -> Result<PathBuf, String> {
    registry
        .0
        .lock()
        .map_err(|_| "folder selection registry poisoned".to_string())?
        .get(selection_id)
        .filter(|selection| selection.purpose == purpose)
        .map(|selection| selection.path.clone())
        .ok_or_else(|| "folder_selection_not_found".to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_scan(
    app: tauri::AppHandle,
    application: tauri::State<'_, ApplicationState>,
    registry: tauri::State<'_, ScanRegistry>,
    folders: tauri::State<'_, FolderSelectionRegistry>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    source_selection_id: String,
    workers: Option<usize>,
    expected_workflow_generation: u64,
) -> Result<ScanStatus, String> {
    validate_opaque_id(&source_selection_id)?;
    let source = selected_folder(folders.inner(), &source_selection_id, FolderPurpose::Source)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let token = CancellationToken::default();
    let (workflow_generation, superseded_job_id) = {
        let mut workflow = session
            .0
            .lock()
            .map_err(|_| "workflow session poisoned".to_string())?;
        if workflow.workflow_generation != expected_workflow_generation {
            return Err("workflow_generation_stale".into());
        }
        if workflow.active_scan_request.is_some() {
            return Err("scan_already_running".into());
        }
        if workflow.active_job_id.is_some() {
            return Err("workflow_job_busy".into());
        }
        if workflow.active_mutation_job_id.is_some() || jobs.has_active_mutation()? {
            return Err("mutation_job_busy".into());
        }
        let generation = workflow.workflow_generation.saturating_add(1);
        let superseded_job_id = workflow.active_job_id.clone();
        *workflow = WorkflowSessionState {
            workflow_generation: generation,
            active_scan_request: Some(request_id.clone()),
            ..WorkflowSessionState::default()
        };
        (generation, superseded_job_id)
    };
    if let Some(job_id) = superseded_job_id {
        jobs.supersede(&job_id)?;
    }
    let initial = ScanStatus {
        request_id: request_id.clone(),
        workflow_generation,
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
    let store = application.store.clone();
    let request_for_thread = request_id.clone();
    let session = session.inner().clone();
    std::thread::spawn(move || {
        let mut options = ScanOptions::default();
        if let Some(value) = workers {
            options.workers = value.clamp(1, 64);
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
                    let _ = app_for_progress.emit(
                        "scan-progress",
                        ScanProgressEvent {
                            request_id: request_for_progress.clone(),
                            workflow_generation,
                            progress,
                        },
                    );
                }
            }
        }));
        let result = ScanUseCase {
            fs: Arc::new(LocalFileSystem),
            metadata: Arc::new(LoftyMetadataReader),
            store,
        }
        .execute(&source, &options)
        .map_err(|error| error.to_string());
        let mut status = match result {
            Ok(result) => ScanStatus {
                request_id: request_for_thread.clone(),
                workflow_generation,
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
                workflow_generation,
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
        if let Ok(mut workflow) = session.0.lock() {
            if workflow.workflow_generation == workflow_generation
                && workflow.active_scan_request.as_deref() == Some(&request_for_thread)
            {
                workflow.active_scan_request = None;
                workflow.scan_id = (status.status == "completed")
                    .then(|| status.scan_id.clone())
                    .flatten();
                workflow.plan_id = None;
                workflow.execution_id = None;
                workflow.view_only = false;
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
    validate_opaque_id(&request_id)?;
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
    validate_opaque_id(&scan_id)?;
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
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn create_plan(
    application: tauri::State<'_, ApplicationState>,
    folders: tauri::State<'_, FolderSelectionRegistry>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    scan_id: String,
    target_selection_id: String,
    naming: Option<music_folder_core::NamingRules>,
    expected_workflow_generation: u64,
) -> Result<JobSnapshot, String> {
    validate_opaque_id(&scan_id)?;
    validate_opaque_id(&target_selection_id)?;
    let target = selected_folder(folders.inner(), &target_selection_id, FolderPurpose::Target)?;
    let job = begin_job(
        session.inner(),
        jobs.inner(),
        expected_workflow_generation,
        JobKind::Plan,
        false,
        &scan_id,
    )?;
    let store = application.store.clone();
    let work_scan_id = scan_id;
    let options = PlanOptions {
        target_root: target,
        batch_size: 250,
        naming: naming.unwrap_or_default(),
    };
    run_job(
        session.inner().clone(),
        jobs.inner().clone(),
        job.clone(),
        move || {
            let result = PlanUseCase { store }
                .execute(&work_scan_id, &options)
                .map_err(|error| error.to_string())?;
            Ok(WorkflowResponse {
                id: result.plan_id,
                success: result.items,
                skipped: result.conflicts,
                failed: result.risks,
            })
        },
    );
    Ok(job)
}
#[tauri::command]
fn preview_naming(naming: music_folder_core::NamingRules) -> music_folder_core::NamingPreview {
    music_folder_core::preview_naming(
        &naming,
        &music_folder_core::TrackMetadata {
            artist: Some("サンプルアーティスト".into()),
            album_artist: Some("サンプルアルバムアーティスト".into()),
            album: Some("サンプルアルバム".into()),
            title: Some("サンプル曲".into()),
            track_no: Some(3),
            disc_no: Some(1),
            year: Some(2026),
        },
    )
}

#[tauri::command]
fn authorize_plan_target(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    authorizations: tauri::State<'_, PlanTargetAuthorizationRegistry>,
    plan_id: String,
    plan_item_id: String,
    target: String,
    expected_workflow_generation: u64,
) -> Result<PlanTargetAuthorizationResponse, String> {
    validate_opaque_id(&plan_id)?;
    validate_opaque_id(&plan_item_id)?;
    if target.len() > 128 * 1024 || target.contains('\0') {
        return Err("manual_target_payload_invalid".into());
    }
    {
        let workflow = session
            .0
            .lock()
            .map_err(|_| "workflow session poisoned".to_string())?;
        if workflow.workflow_generation != expected_workflow_generation {
            return Err("workflow_generation_stale".into());
        }
        if workflow.view_only
            || workflow.active_scan_request.is_some()
            || workflow.active_job_id.is_some()
            || workflow.plan_id.as_deref() != Some(&plan_id)
        {
            return Err("workflow_context_stale".into());
        }
    }
    let validated = application.store.validate_manual_target_for_plan(
        &plan_id,
        &plan_item_id,
        &PathBuf::from(target),
    )?;
    authorizations.issue(
        plan_id,
        plan_item_id,
        expected_workflow_generation,
        validated,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn authorize_plan_conflict_candidate(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    authorizations: tauri::State<'_, PlanTargetAuthorizationRegistry>,
    plan_id: String,
    plan_item_id: String,
    conflict_group_id: String,
    candidate_ordinal: u64,
    expected_workflow_generation: u64,
) -> Result<PlanTargetAuthorizationResponse, String> {
    validate_opaque_id(&plan_id)?;
    validate_opaque_id(&plan_item_id)?;
    validate_opaque_id(&conflict_group_id)?;
    {
        let workflow = session
            .0
            .lock()
            .map_err(|_| "workflow session poisoned".to_string())?;
        if workflow.workflow_generation != expected_workflow_generation {
            return Err("workflow_generation_stale".into());
        }
        if workflow.view_only
            || workflow.active_scan_request.is_some()
            || workflow.active_job_id.is_some()
            || workflow.plan_id.as_deref() != Some(&plan_id)
        {
            return Err("workflow_context_stale".into());
        }
    }
    let validated = application.store.resolve_plan_conflict_candidate_target(
        &plan_id,
        &plan_item_id,
        &conflict_group_id,
        candidate_ordinal,
    )?;
    authorizations.issue(
        plan_id,
        plan_item_id,
        expected_workflow_generation,
        validated,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn revise_plan_target(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    authorizations: tauri::State<'_, PlanTargetAuthorizationRegistry>,
    plan_id: String,
    plan_item_id: String,
    target_capability_id: String,
    expected_workflow_generation: u64,
) -> Result<JobSnapshot, String> {
    validate_opaque_id(&plan_id)?;
    validate_opaque_id(&plan_item_id)?;
    validate_opaque_id(&target_capability_id)?;
    let target = authorizations.consume(
        &target_capability_id,
        &plan_id,
        &plan_item_id,
        expected_workflow_generation,
    )?;
    let job = begin_job(
        session.inner(),
        jobs.inner(),
        expected_workflow_generation,
        JobKind::PlanRevision,
        false,
        &plan_id,
    )?;
    let store = application.store.clone();
    run_job(
        session.inner().clone(),
        jobs.inner().clone(),
        job.clone(),
        move || {
            let id = music_folder_core::usecases::RevisePlanUseCase { store }
                .execute(
                    &plan_id,
                    &[ManualTargetChange {
                        plan_item_id,
                        target,
                        reason: "manual_target".into(),
                    }],
                )
                .map_err(|error| error.to_string())?;
            Ok(WorkflowResponse {
                id,
                success: 0,
                skipped: 0,
                failed: 0,
            })
        },
    );
    Ok(job)
}
#[tauri::command]
fn apply_plan(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    plan_id: String,
    execute: bool,
    confirm: Option<String>,
    expected_workflow_generation: u64,
) -> Result<JobSnapshot, String> {
    validate_opaque_id(&plan_id)?;
    require_execute_confirmation(execute, confirm.as_deref(), &plan_id)?;
    ensure_no_pending_recovery(application.inner())?;
    let job = begin_job(
        session.inner(),
        jobs.inner(),
        expected_workflow_generation,
        JobKind::Apply,
        execute,
        &plan_id,
    )?;
    let store = application.store.clone();
    run_job(
        session.inner().clone(),
        jobs.inner().clone(),
        job.clone(),
        move || {
            let result = ApplyUseCase {
                store,
                files: Arc::new(LocalFileSystem),
            }
            .execute(&plan_id, !execute)
            .map_err(|error| error.to_string())?;
            Ok(WorkflowResponse {
                id: result.execution_id,
                success: result.success,
                skipped: result.skipped,
                failed: result.failed,
            })
        },
    );
    Ok(job)
}
#[tauri::command]
fn verify_execution(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    execution_id: String,
    expected_workflow_generation: u64,
) -> Result<JobSnapshot, String> {
    validate_opaque_id(&execution_id)?;
    let job = begin_job(
        session.inner(),
        jobs.inner(),
        expected_workflow_generation,
        JobKind::Verify,
        false,
        &execution_id,
    )?;
    let store = application.store.clone();
    run_job(
        session.inner().clone(),
        jobs.inner().clone(),
        job.clone(),
        move || {
            let result = VerifyUseCase {
                store,
                files: Arc::new(LocalFileSystem),
            }
            .execute(&execution_id)
            .map_err(|error| error.to_string())?;
            Ok(WorkflowResponse {
                id: result.verify_id,
                success: result.success,
                skipped: 0,
                failed: result.failed,
            })
        },
    );
    Ok(job)
}
#[tauri::command]
fn rollback_execution(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    execution_id: String,
    execute: bool,
    confirm: Option<String>,
    expected_workflow_generation: u64,
) -> Result<JobSnapshot, String> {
    validate_opaque_id(&execution_id)?;
    require_execute_confirmation(execute, confirm.as_deref(), &execution_id)?;
    ensure_no_pending_recovery(application.inner())?;
    let job = begin_job(
        session.inner(),
        jobs.inner(),
        expected_workflow_generation,
        JobKind::Rollback,
        execute,
        &execution_id,
    )?;
    let store = application.store.clone();
    run_job(
        session.inner().clone(),
        jobs.inner().clone(),
        job.clone(),
        move || {
            let result = RollbackUseCase {
                store,
                files: Arc::new(LocalFileSystem),
            }
            .execute(&execution_id, !execute)
            .map_err(|error| error.to_string())?;
            Ok(WorkflowResponse {
                id: result.rollback_id,
                success: result.success,
                skipped: result.skipped,
                failed: result.failed,
            })
        },
    );
    Ok(job)
}

#[tauri::command]
fn get_job_snapshot(
    jobs: tauri::State<'_, JobRegistry>,
    job_id: String,
    after_seq: Option<u64>,
) -> Result<Option<JobSnapshot>, String> {
    validate_opaque_id(&job_id)?;
    jobs.snapshot(&job_id, after_seq)
}

#[tauri::command]
fn list_active_jobs(
    jobs: tauri::State<'_, JobRegistry>,
    session: tauri::State<'_, WorkflowSession>,
) -> Result<Vec<JobSnapshot>, String> {
    // Keep the latest terminal snapshot discoverable as well: a fast job can
    // finish after the backend accepts it but before the WebView persists the
    // returned handle.
    let (latest_job_id, workflow_generation) = {
        let workflow = session
            .0
            .lock()
            .map_err(|_| "workflow session poisoned".to_string())?;
        (
            workflow
                .active_job_id
                .clone()
                .or_else(|| workflow.latest_job_id.clone()),
            workflow.workflow_generation,
        )
    };
    let Some(job_id) = latest_job_id else {
        return Ok(Vec::new());
    };
    let Some(snapshot) = jobs.snapshot(&job_id, None)? else {
        return Ok(Vec::new());
    };
    Ok((snapshot.workflow_generation == workflow_generation)
        .then_some(snapshot)
        .into_iter()
        .collect())
}

fn ensure_no_pending_recovery(application: &ApplicationState) -> Result<(), String> {
    if application.store.list_recovery_items()?.is_empty() {
        Ok(())
    } else {
        Err("recovery_required".into())
    }
}

fn recovery_action(value: &str) -> Result<RecoveryAction, String> {
    match value {
        "resume" => Ok(RecoveryAction::Resume),
        "rollback_published" => Ok(RecoveryAction::RollbackPublished),
        "discard_unpublished_temporary" => Ok(RecoveryAction::DiscardUnpublishedTemporary),
        "manual_intervention" => Ok(RecoveryAction::ManualIntervention),
        _ => Err("recovery_action_invalid".into()),
    }
}

#[tauri::command]
fn list_recovery_items(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
) -> Result<Vec<RecoveryItemResponse>, String> {
    let items = RecoveryUseCase {
        store: application.store.clone(),
        files: Arc::new(LocalFileSystem),
    }
    .inspect()
    .map_err(|error| error.to_string())?;
    let allowed = items
        .iter()
        .map(|item| item.operation_id.clone())
        .collect::<HashSet<_>>();
    session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?
        .recovery_operation_ids = allowed;
    Ok(items
        .into_iter()
        .map(|item| RecoveryItemResponse {
            operation_id: item.operation_id,
            attempt_id: item.attempt_id,
            sequence_no: item.sequence_no,
            state: item.state.as_str().into(),
            source: music_folder_infra::path_codec::path_envelope(&item.source, "recovery_source"),
            target: music_folder_infra::path_codec::path_envelope(&item.target, "recovery_target"),
            temporary: music_folder_infra::path_codec::path_envelope(
                &item.temporary,
                "recovery_temporary",
            ),
            expected_size: item.expected_size,
            suggested_action: item.suggested_action.as_str().into(),
        })
        .collect())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn run_recovery(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    operation_id: String,
    action: String,
    execute: bool,
    confirm: Option<String>,
    expected_workflow_generation: u64,
) -> Result<RecoveryWorkflowResponse, String> {
    validate_opaque_id(&operation_id)?;
    require_execute_confirmation(execute, confirm.as_deref(), &operation_id)
        .map_err(|_| "recovery_confirmation_required".to_string())?;
    let workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    ensure_idle_workflow_context(&workflow, jobs.inner(), expected_workflow_generation)?;
    let mutation_guard = reserve_external_mutation(jobs.inner())?;
    drop(workflow);
    let is_current = application
        .store
        .list_recovery_items()?
        .into_iter()
        .any(|item| item.operation_id == operation_id);
    if !is_current {
        return Err("recovery_context_stale".into());
    }
    let action = recovery_action(&action)?;
    let result = RecoveryUseCase {
        store: application.store.clone(),
        files: Arc::new(LocalFileSystem),
    }
    .execute(&operation_id, Some(action), !execute)
    .map_err(|error| error.to_string())?;

    let (verify_id, verify_success, verify_failed) = if execute {
        let verified = VerifyUseCase {
            store: application.store.clone(),
            files: Arc::new(LocalFileSystem),
        }
        .execute_subject(&result.recovery_id, VerifySubject::Recovery)
        .map_err(|error| error.to_string())?;
        (Some(verified.verify_id), verified.success, verified.failed)
    } else {
        (None, 0, 0)
    };
    let mut workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    if workflow.workflow_generation != expected_workflow_generation {
        return Err("workflow_generation_stale".into());
    }
    if execute {
        workflow.recovery_operation_ids.remove(&operation_id);
    }
    workflow.workflow_generation = workflow.workflow_generation.saturating_add(1);
    let workflow_generation = workflow.workflow_generation;
    drop(workflow);
    drop(mutation_guard);
    Ok(RecoveryWorkflowResponse {
        recovery_id: result.recovery_id,
        operation_id: result.operation_id,
        action: result.action.as_str().into(),
        dry_run: result.dry_run,
        verify_id,
        verify_success,
        verify_failed,
        workflow_generation,
    })
}
#[tauri::command]
fn invalidate_workflow(
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    keep_scan: bool,
    expected_workflow_generation: u64,
) -> Result<u64, String> {
    let mut workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    ensure_idle_workflow_context(&workflow, jobs.inner(), expected_workflow_generation)?;
    let scan_id = keep_scan.then(|| workflow.scan_id.clone()).flatten();
    let generation = workflow.workflow_generation.saturating_add(1);
    *workflow = WorkflowSessionState {
        workflow_generation: generation,
        scan_id,
        ..WorkflowSessionState::default()
    };
    Ok(generation)
}

#[tauri::command]
fn activate_history(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    kind: String,
    run_id: String,
    expected_workflow_generation: u64,
) -> Result<u64, String> {
    validate_history_kind(&kind)?;
    validate_opaque_id(&run_id)?;
    let selected = application.store.get_run_detail(&kind, &run_id)?;
    let (scan_id, plan_id, execution_id) = match kind.as_str() {
        "scan" => (run_id, None, None),
        "plan" => {
            let scan_id = selected
                .parent_id
                .clone()
                .ok_or_else(|| "history_scan_parent_missing".to_string())?;
            (scan_id, Some(run_id), None)
        }
        "apply" => {
            let plan_id = selected
                .parent_id
                .clone()
                .ok_or_else(|| "history_plan_parent_missing".to_string())?;
            let plan = application.store.get_run_detail("plan", &plan_id)?;
            let scan_id = plan
                .parent_id
                .ok_or_else(|| "history_scan_parent_missing".to_string())?;
            (scan_id, Some(plan_id), Some(run_id))
        }
        "verify" | "rollback" => {
            let execution_id = selected
                .parent_id
                .clone()
                .ok_or_else(|| "history_execution_parent_missing".to_string())?;
            let execution = application.store.get_run_detail("apply", &execution_id)?;
            let plan_id = execution
                .parent_id
                .ok_or_else(|| "history_plan_parent_missing".to_string())?;
            let plan = application.store.get_run_detail("plan", &plan_id)?;
            let scan_id = plan
                .parent_id
                .ok_or_else(|| "history_scan_parent_missing".to_string())?;
            (scan_id, Some(plan_id), Some(execution_id))
        }
        _ => return Err("invalid_run_kind".into()),
    };
    let mut workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    ensure_idle_workflow_context(&workflow, jobs.inner(), expected_workflow_generation)?;
    let generation = workflow.workflow_generation.saturating_add(1);
    *workflow = WorkflowSessionState {
        workflow_generation: generation,
        scan_id: Some(scan_id),
        plan_id,
        execution_id,
        view_only: selected.status == "archived",
        ..WorkflowSessionState::default()
    };
    Ok(generation)
}
#[tauri::command]
fn delete_history(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    kind: String,
    run_id: String,
    confirm: String,
    expected_workflow_generation: u64,
) -> Result<(), String> {
    validate_history_kind(&kind)?;
    validate_opaque_id(&run_id)?;
    validate_opaque_id(&confirm)?;
    if kind != "scan" || confirm != run_id {
        return Err("history_purge_confirmation_required".into());
    }
    let mut workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    ensure_idle_workflow_context(&workflow, jobs.inner(), expected_workflow_generation)?;
    application.store.delete_history(&kind, &run_id)?;
    let generation = workflow.workflow_generation.saturating_add(1);
    *workflow = WorkflowSessionState {
        workflow_generation: generation,
        ..WorkflowSessionState::default()
    };
    Ok(())
}
#[tauri::command]
fn archive_history(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    jobs: tauri::State<'_, JobRegistry>,
    kind: String,
    run_id: String,
    confirm: String,
    expected_workflow_generation: u64,
) -> Result<music_folder_infra::sqlite::HistoryArchiveResult, String> {
    validate_history_kind(&kind)?;
    validate_opaque_id(&run_id)?;
    validate_opaque_id(&confirm)?;
    let preview = application.store.history_cleanup_preview(&kind, &run_id)?;
    if confirm != preview.root_scan_id {
        return Err("history_archive_confirmation_required".into());
    }
    let mut workflow = session
        .0
        .lock()
        .map_err(|_| "workflow session poisoned".to_string())?;
    ensure_idle_workflow_context(&workflow, jobs.inner(), expected_workflow_generation)?;
    let result = application.store.archive_history(&kind, &run_id, None)?;
    let generation = workflow.workflow_generation.saturating_add(1);
    *workflow = WorkflowSessionState {
        workflow_generation: generation,
        ..WorkflowSessionState::default()
    };
    Ok(result)
}
#[tauri::command]
fn history_cleanup_preview(
    application: tauri::State<'_, ApplicationState>,
    kind: String,
    run_id: String,
) -> Result<music_folder_infra::sqlite::HistoryCleanupPreview, String> {
    validate_history_kind(&kind)?;
    validate_opaque_id(&run_id)?;
    application.store.history_cleanup_preview(&kind, &run_id)
}
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn list_history(
    application: tauri::State<'_, ApplicationState>,
    limit: u32,
    cursor_started_at: Option<i64>,
    cursor_id: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    query: Option<String>,
    oldest_first: Option<bool>,
) -> Result<Vec<music_folder_infra::sqlite::HistoryRow>, String> {
    if let Some(value) = kind.as_deref() {
        validate_history_kind(value)?;
    }
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "running" | "completed" | "failed" | "cancelled" | "recovery_required" | "archived"
        )
    }) {
        return Err("invalid_run_status".into());
    }
    if let Some(value) = cursor_id.as_deref() {
        validate_opaque_id(value)?;
    }
    validate_optional_query(query.as_deref())?;
    application.store.list_history_filtered(
        limit.clamp(1, 200),
        cursor_started_at,
        cursor_id.as_deref(),
        kind.as_deref(),
        status.as_deref(),
        query.as_deref(),
        oldest_first.unwrap_or(false),
    )
}
#[tauri::command]
fn get_run_detail(
    application: tauri::State<'_, ApplicationState>,
    kind: String,
    run_id: String,
) -> Result<music_folder_infra::sqlite::RunDetailRow, String> {
    validate_history_kind(&kind)?;
    validate_opaque_id(&run_id)?;
    application.store.get_run_detail(&kind, &run_id)
}
#[tauri::command]
fn list_attempt_evidence(
    application: tauri::State<'_, ApplicationState>,
    kind: String,
    run_id: String,
) -> Result<music_folder_infra::sqlite::AttemptEvidence, String> {
    validate_history_kind(&kind)?;
    validate_opaque_id(&run_id)?;
    // Validate the requested display kind first so an arbitrary attempt ID
    // cannot be used as an enumeration oracle for a different run class.
    application.store.get_run_detail(&kind, &run_id)?;
    application.store.list_attempt_evidence(&run_id)
}
#[tauri::command]
fn list_plan_items(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    plan_id: String,
    cursor: Option<u64>,
    limit: u32,
    query: Option<String>,
    risk: Option<String>,
) -> Result<music_folder_infra::sqlite::PlanItemPage, String> {
    validate_opaque_id(&plan_id)?;
    validate_optional_query(query.as_deref())?;
    if risk.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "none" | "conflict" | "invalid_target" | "metadata_missing" | "path_too_long"
        )
    }) {
        return Err("invalid_plan_risk".into());
    }
    session.can_view_plan(&plan_id)?;
    application.store.list_plan_items(
        &plan_id,
        cursor,
        limit.clamp(1, 200),
        query.as_deref(),
        risk.as_deref(),
    )
}
#[tauri::command]
fn get_plan_conflict_detail(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    plan_id: String,
    conflict_group_id: String,
) -> Result<music_folder_infra::sqlite::PlanConflictDetail, String> {
    validate_opaque_id(&plan_id)?;
    validate_opaque_id(&conflict_group_id)?;
    query_plan_conflict_detail(
        application.inner(),
        session.inner(),
        &plan_id,
        &conflict_group_id,
    )
}

fn query_plan_conflict_detail(
    application: &ApplicationState,
    session: &WorkflowSession,
    plan_id: &str,
    conflict_group_id: &str,
) -> Result<music_folder_infra::sqlite::PlanConflictDetail, String> {
    session.can_view_plan(plan_id)?;
    application
        .store
        .get_plan_conflict_detail(plan_id, conflict_group_id)
}

fn prepare_private_state_directory(directory: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(directory) {
        Ok(metadata) => validate_private_state_entry(directory, &metadata, true)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(directory).map_err(|create_error| {
                format!("desktop_state_directory_create_failed:{create_error}")
            })?;
            let metadata = std::fs::symlink_metadata(directory).map_err(|stat_error| {
                format!("desktop_state_directory_stat_failed:{stat_error}")
            })?;
            validate_private_state_entry(directory, &metadata, true)?;
        }
        Err(error) => return Err(format!("desktop_state_directory_stat_failed:{error}")),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("desktop_state_directory_permission_failed:{error}"))?;
    }
    #[cfg(windows)]
    harden_private_state_acl(directory, true)?;
    Ok(())
}

fn validate_private_state_database(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_state_entry(path, &metadata, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("desktop_state_database_stat_failed:{error}")),
    }
}

fn harden_private_state_database(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("desktop_state_database_stat_failed:{error}"))?;
    validate_private_state_entry(path, &metadata, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("desktop_state_database_permission_failed:{error}"))?;
    }
    #[cfg(windows)]
    harden_private_state_acl(path, false)?;
    Ok(())
}

#[cfg(windows)]
fn harden_private_state_acl(path: &Path, directory: bool) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC,
        },
    };

    let user_sid = current_user_sid_string()?;
    let inheritance = if directory { "OICI" } else { "" };
    let sddl = format!(
        "D:P(A;{inheritance};FA;;;SY)(A;{inheritance};FA;;;BA)(A;{inheritance};FA;;;{user_sid})"
    );
    let sddl_wide = sddl
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: `sddl_wide` is NUL terminated and the out pointer is valid. The
    // descriptor is released with LocalFree on every subsequent path.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!(
            "desktop_state_acl_descriptor_failed:{}",
            std::io::Error::last_os_error()
        ));
    }

    let result = (|| {
        let mut dacl_present = 0;
        let mut dacl_defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: the converted descriptor remains allocated for the closure,
        // and all out pointers refer to initialized local variables.
        if unsafe {
            GetSecurityDescriptorDacl(
                descriptor,
                &mut dacl_present,
                &mut dacl,
                &mut dacl_defaulted,
            )
        } == 0
            || dacl_present == 0
            || dacl.is_null()
        {
            return Err(format!(
                "desktop_state_acl_extract_failed:{}",
                std::io::Error::last_os_error()
            ));
        }

        let access = READ_CONTROL | WRITE_DAC;
        let flags = FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
        let file = OpenOptions::new()
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(flags)
            .open(path)
            .map_err(|error| format!("desktop_state_acl_open_failed:{error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("desktop_state_acl_stat_failed:{error}"))?;
        validate_private_state_entry(path, &metadata, directory)?;

        // SAFETY: the handle is owned by `file`, the DACL belongs to the live
        // descriptor, and null owner/group/SACL pointers are required because
        // only the protected DACL is being replaced.
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(format!(
                "desktop_state_acl_apply_failed:{}",
                std::io::Error::from_raw_os_error(status as i32)
            ));
        }
        Ok(())
    })();

    // SAFETY: the descriptor was allocated by LocalAlloc inside the SDDL API.
    unsafe {
        LocalFree(descriptor);
    }
    result
}

#[cfg(windows)]
fn current_user_sid_string() -> Result<String, String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER},
        Security::Authorization::ConvertSidToStringSidW,
        Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    struct TokenHandle(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by OpenProcessToken and is owned
            // by this guard.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let mut token = std::ptr::null_mut();
    // SAFETY: the pseudo process handle is always valid and the output pointer
    // refers to an initialized local variable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!(
            "desktop_state_user_token_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    let token = TokenHandle(token);

    let mut required = 0;
    // SAFETY: a null buffer with zero length is the documented size query.
    let first =
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut required) };
    // SAFETY: GetLastError reads thread-local state from the immediately
    // preceding failed Windows API call.
    let size_error = unsafe { GetLastError() };
    if first != 0 || required == 0 || size_error != ERROR_INSUFFICIENT_BUFFER {
        return Err(format!(
            "desktop_state_user_token_size_failed:{}",
            std::io::Error::from_raw_os_error(size_error as i32)
        ));
    }

    let mut buffer = vec![0_u8; required as usize];
    // SAFETY: `buffer` has the size requested by the API and remains alive
    // while the TOKEN_USER and SID pointers are used.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(format!(
            "desktop_state_user_token_read_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: a successful TokenUser query returns a properly aligned
    // TOKEN_USER at the start of the buffer allocated above.
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid_text = std::ptr::null_mut();
    // SAFETY: the token SID is valid for the lifetime of `buffer`; the output
    // is released with LocalFree below.
    if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_text) } == 0 {
        return Err(format!(
            "desktop_state_user_sid_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    let result = {
        let mut length = 0;
        // SAFETY: ConvertSidToStringSidW returns a NUL-terminated UTF-16 string.
        while unsafe { *sid_text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: `length` was obtained by scanning the allocated string up to
        // its NUL terminator.
        String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, length) })
            .map_err(|error| format!("desktop_state_user_sid_decode_failed:{error}"))
    };
    // SAFETY: sid_text was allocated by LocalAlloc inside the conversion API.
    unsafe {
        LocalFree(sid_text.cast());
    }
    result
}

#[cfg(all(test, windows))]
fn private_state_acl_sddl(path: &Path) -> Result<String, String> {
    use std::fs::OpenOptions;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
        },
    };

    let file = OpenOptions::new()
        .access_mode(READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|error| format!("desktop_state_acl_test_open_failed:{error}"))?;
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: `file` owns a valid handle and every unused output is null. The
    // returned descriptor is freed below.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(format!(
            "desktop_state_acl_test_read_failed:{}",
            std::io::Error::from_raw_os_error(status as i32)
        ));
    }

    let result = (|| {
        let mut text = std::ptr::null_mut();
        // SAFETY: the descriptor remains valid and the output pointer is local.
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "desktop_state_acl_test_encode_failed:{}",
                std::io::Error::last_os_error()
            ));
        }
        let encoded = {
            let mut length = 0;
            // SAFETY: the conversion API returns a NUL-terminated UTF-16 string.
            while unsafe { *text.add(length) } != 0 {
                length += 1;
            }
            // SAFETY: the slice ends before the NUL terminator found above.
            String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
                .map_err(|error| format!("desktop_state_acl_test_decode_failed:{error}"))
        };
        // SAFETY: the conversion API allocates this string with LocalAlloc.
        unsafe {
            LocalFree(text.cast());
        }
        encoded
    })();
    // SAFETY: GetSecurityInfo allocates the descriptor with LocalAlloc.
    unsafe {
        LocalFree(descriptor);
    }
    result
}

fn validate_private_state_entry(
    path: &Path,
    metadata: &std::fs::Metadata,
    directory: bool,
) -> Result<(), String> {
    if metadata_is_reparse(metadata) {
        return Err(format!(
            "desktop_state_reparse_forbidden:{}",
            path.display()
        ));
    }
    let expected_kind = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_kind {
        return Err(if directory {
            "desktop_state_directory_not_directory".into()
        } else {
            "desktop_state_database_not_file".into()
        });
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[tauri::command]
fn list_operation_logs(
    application: tauri::State<'_, ApplicationState>,
    session: tauri::State<'_, WorkflowSession>,
    execution_id: String,
    cursor: Option<u64>,
    limit: u32,
    query: Option<String>,
    result: Option<String>,
) -> Result<Vec<music_folder_infra::sqlite::OperationLogRow>, String> {
    validate_opaque_id(&execution_id)?;
    validate_optional_query(query.as_deref())?;
    if result
        .as_deref()
        .is_some_and(|value| !matches!(value, "success" | "skipped" | "failed"))
    {
        return Err("invalid_operation_result".into());
    }
    session.can_view_execution(&execution_id)?;
    application.store.list_operation_logs(
        &execution_id,
        cursor,
        limit.clamp(1, 200),
        query.as_deref(),
        result.as_deref(),
    )
}
#[tauri::command]
fn list_metrics(
    application: tauri::State<'_, ApplicationState>,
    run_id: String,
) -> Result<Vec<music_folder_infra::sqlite::MetricRow>, String> {
    validate_opaque_id(&run_id)?;
    application.store.list_metrics(&run_id)
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(ScanRegistry::default())
        .manage(JobRegistry::default())
        .manage(WorkflowSession::default())
        .manage(PlanTargetAuthorizationRegistry::default())
        .setup(|app| {
            let directory = app.path().app_local_data_dir()?;
            prepare_private_state_directory(&directory).map_err(std::io::Error::other)?;
            let database_path = directory.join("music-folder.db");
            validate_private_state_database(&database_path).map_err(std::io::Error::other)?;
            let store = SqliteScanStore::open(&database_path).map_err(std::io::Error::other)?;
            // Revalidate after SQLite has created/opened the file so a link or
            // non-regular replacement never becomes the managed authority.
            harden_private_state_database(&database_path).map_err(std::io::Error::other)?;
            app.manage(ApplicationState {
                store: Arc::new(store),
            });
            app.manage(FolderSelectionRegistry::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_backend_ready,
            get_workflow_capabilities,
            pick_folder,
            start_scan,
            scan_status,
            cancel_scan,
            create_plan,
            preview_naming,
            authorize_plan_target,
            authorize_plan_conflict_candidate,
            revise_plan_target,
            apply_plan,
            verify_execution,
            rollback_execution,
            get_job_snapshot,
            list_active_jobs,
            list_recovery_items,
            run_recovery,
            invalidate_workflow,
            activate_history,
            archive_history,
            delete_history,
            history_cleanup_preview,
            list_history,
            get_run_detail,
            list_attempt_evidence,
            list_plan_items,
            get_plan_conflict_detail,
            list_operation_logs,
            list_metrics
        ])
        .run(tauri::generate_context!())
        .expect("failed to run desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn private_state_boundary_creates_regular_private_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("application-state");
        prepare_private_state_directory(&directory).unwrap();
        assert!(directory.is_dir());

        let database = directory.join("state.sqlite3");
        validate_private_state_database(&database).unwrap();
        fs::write(&database, b"sqlite fixture").unwrap();
        harden_private_state_database(&database).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&database).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        {
            let user_sid = current_user_sid_string().unwrap();
            for path in [&directory, &database] {
                let sddl = private_state_acl_sddl(path).unwrap();
                assert!(sddl.starts_with("D:P"), "DACL must be protected: {sddl}");
                assert!(sddl.contains(&user_sid), "current user missing: {sddl}");
                assert!(sddl.contains(";;;SY)"), "SYSTEM missing: {sddl}");
                assert!(sddl.contains(";;;BA)"), "Administrators missing: {sddl}");
                assert!(!sddl.contains(";;;WD)"), "Everyone must be absent: {sddl}");
                assert!(
                    !sddl.contains(";;;AU)"),
                    "Authenticated Users must be absent: {sddl}"
                );
                assert!(!sddl.contains(";;;BU)"), "Users must be absent: {sddl}");
            }
        }
    }

    #[test]
    fn private_state_boundary_rejects_wrong_entry_kinds() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("not-a-directory");
        fs::write(&file, b"fixture").unwrap();
        assert_eq!(
            prepare_private_state_directory(&file).unwrap_err(),
            "desktop_state_directory_not_directory"
        );

        let directory = temporary.path().join("not-a-database");
        fs::create_dir(&directory).unwrap();
        assert_eq!(
            validate_private_state_database(&directory).unwrap_err(),
            "desktop_state_database_not_file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_state_boundary_rejects_symbolic_link_replacement() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let actual = temporary.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let linked = temporary.path().join("linked");
        symlink(&actual, &linked).unwrap();
        assert!(prepare_private_state_directory(&linked)
            .unwrap_err()
            .starts_with("desktop_state_reparse_forbidden:"));
    }

    #[test]
    fn opaque_folder_selection_is_bound_to_its_native_picker_purpose() {
        let registry = FolderSelectionRegistry::default();
        registry.0.lock().unwrap().insert(
            "source-token".into(),
            FolderSelection {
                path: PathBuf::from(r"C:\Music\Incoming"),
                purpose: FolderPurpose::Source,
            },
        );
        assert_eq!(
            selected_folder(&registry, "source-token", FolderPurpose::Source).unwrap(),
            PathBuf::from(r"C:\Music\Incoming")
        );
        assert_eq!(
            selected_folder(&registry, "source-token", FolderPurpose::Target).unwrap_err(),
            "folder_selection_not_found"
        );
        assert_eq!(
            selected_folder(&registry, r"C:\forged", FolderPurpose::Source).unwrap_err(),
            "folder_selection_not_found"
        );
    }

    #[test]
    fn manual_target_capability_is_bound_one_shot_and_rejects_tampering() {
        let registry = PlanTargetAuthorizationRegistry::default();
        let tampered = registry
            .issue(
                "plan-1".into(),
                "item-1".into(),
                7,
                PathBuf::from(r"C:\Organized\album\song.flac"),
            )
            .unwrap();
        assert_eq!(
            registry
                .consume(
                    &format!("{}-forged", tampered.target_capability_id),
                    "plan-1",
                    "item-1",
                    7,
                )
                .unwrap_err(),
            "plan_target_capability_not_found"
        );
        assert_eq!(
            registry
                .consume(&tampered.target_capability_id, "plan-1", "item-1", 7,)
                .unwrap(),
            PathBuf::from(r"C:\Organized\album\song.flac")
        );

        let wrong_generation = registry
            .issue(
                "plan-1".into(),
                "item-1".into(),
                7,
                PathBuf::from(r"C:\Organized\album\song.flac"),
            )
            .unwrap();
        assert_eq!(
            registry
                .consume(
                    &wrong_generation.target_capability_id,
                    "plan-1",
                    "item-1",
                    8,
                )
                .unwrap_err(),
            "plan_target_capability_mismatch"
        );
        assert_eq!(
            registry
                .consume(
                    &wrong_generation.target_capability_id,
                    "plan-1",
                    "item-1",
                    7,
                )
                .unwrap_err(),
            "plan_target_capability_not_found"
        );

        let forged = registry
            .issue(
                "plan-1".into(),
                "item-1".into(),
                7,
                PathBuf::from(r"C:\Organized\album\song.flac"),
            )
            .unwrap();
        assert_eq!(
            registry
                .consume(&forged.target_capability_id, "plan-forged", "item-1", 7,)
                .unwrap_err(),
            "plan_target_capability_mismatch"
        );
        assert_eq!(
            registry
                .consume(&forged.target_capability_id, "plan-1", "item-1", 7)
                .unwrap_err(),
            "plan_target_capability_not_found"
        );

        let valid = registry
            .issue(
                "plan-1".into(),
                "item-1".into(),
                7,
                PathBuf::from(r"C:\Organized\album\song.flac"),
            )
            .unwrap();
        assert_eq!(
            registry
                .consume(&valid.target_capability_id, "plan-1", "item-1", 7)
                .unwrap(),
            PathBuf::from(r"C:\Organized\album\song.flac")
        );
        assert_eq!(
            registry
                .consume(&valid.target_capability_id, "plan-1", "item-1", 7)
                .unwrap_err(),
            "plan_target_capability_not_found"
        );
    }

    #[test]
    fn destructive_confirmation_and_request_bounds_fail_closed() {
        assert!(require_execute_confirmation(false, None, "plan-1").is_ok());
        assert_eq!(
            require_execute_confirmation(true, None, "plan-1").unwrap_err(),
            "operation_confirmation_required"
        );
        assert_eq!(
            require_execute_confirmation(true, Some("plan-2"), "plan-1").unwrap_err(),
            "operation_confirmation_required"
        );
        assert!(require_execute_confirmation(true, Some("plan-1"), "plan-1").is_ok());
        assert!(validate_opaque_id("f2a62e16-5a51-44f6-95e2-51c39e768580").is_ok());
        assert_eq!(
            validate_opaque_id(r"C:\forged\plan").unwrap_err(),
            "invalid_command_id"
        );
        assert_eq!(
            validate_opaque_id(&"x".repeat(MAX_COMMAND_ID_BYTES + 1)).unwrap_err(),
            "invalid_command_id"
        );
        assert_eq!(
            validate_optional_query(Some(&"x".repeat(MAX_QUERY_BYTES + 1))).unwrap_err(),
            "command_query_too_large"
        );
    }

    #[test]
    fn workflow_capability_reasons_bind_subject_and_fail_closed() {
        let workflow = WorkflowSessionState {
            plan_id: Some("plan-current".into()),
            execution_id: Some("execution-current".into()),
            ..WorkflowSessionState::default()
        };
        assert!(workflow_action_reasons(
            &workflow,
            false,
            workflow.plan_id.as_deref(),
            false,
            true,
        )
        .is_empty());

        let recovery_blocked = workflow_action_reasons(
            &workflow,
            false,
            workflow.execution_id.as_deref(),
            true,
            true,
        );
        assert_eq!(recovery_blocked, vec!["recovery_required"]);

        let missing_subject = workflow_action_reasons(&workflow, false, None, false, true);
        assert_eq!(missing_subject, vec!["workflow_subject_missing"]);

        let archived = WorkflowSessionState {
            view_only: true,
            active_job_id: Some("job-running".into()),
            ..workflow
        };
        assert_eq!(
            workflow_action_reasons(&archived, false, archived.plan_id.as_deref(), false, true),
            vec!["workflow_archived_read_only", "workflow_job_busy"]
        );
    }

    #[test]
    fn context_changes_reject_active_scan_jobs_and_stale_generations() {
        let jobs = JobRegistry::default();
        let mut workflow = WorkflowSessionState {
            workflow_generation: 9,
            active_scan_request: Some("scan-request".into()),
            ..WorkflowSessionState::default()
        };
        assert_eq!(
            ensure_idle_workflow_context(&workflow, &jobs, 9).unwrap_err(),
            "scan_job_busy"
        );
        workflow.active_scan_request = None;
        assert_eq!(
            ensure_idle_workflow_context(&workflow, &jobs, 8).unwrap_err(),
            "workflow_generation_stale"
        );
        workflow.active_job_id = Some("plan-job".into());
        assert_eq!(
            ensure_idle_workflow_context(&workflow, &jobs, 9).unwrap_err(),
            "workflow_job_busy"
        );
    }

    #[test]
    fn workflow_session_rejects_stale_and_archived_mutation_subjects() {
        let session = WorkflowSession::default();
        let jobs = JobRegistry::default();
        *session.0.lock().unwrap() = WorkflowSessionState {
            scan_id: Some("scan-1".into()),
            plan_id: Some("plan-2".into()),
            execution_id: Some("execution-3".into()),
            ..WorkflowSessionState::default()
        };
        assert_eq!(
            begin_job(&session, &jobs, 0, JobKind::Apply, false, "plan-old").unwrap_err(),
            "workflow_context_stale"
        );
        session.0.lock().unwrap().view_only = true;
        session.can_view_plan("plan-2").unwrap();
        assert_eq!(
            begin_job(&session, &jobs, 0, JobKind::Apply, false, "plan-2").unwrap_err(),
            "workflow_context_stale"
        );
    }

    #[test]
    fn conflict_detail_command_boundary_returns_unloaded_members_and_typed_failures() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(&source).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../infra/tests/fixtures/mp3/japanese.mp3");
        fs::copy(&fixture, source.join("one.mp3")).unwrap();
        fs::copy(&fixture, source.join("two.mp3")).unwrap();
        let database_path = temp.path().join("desktop.sqlite3");
        let store = Arc::new(SqliteScanStore::open(&database_path).unwrap());
        let scan = ScanUseCase {
            fs: Arc::new(LocalFileSystem),
            metadata: Arc::new(LoftyMetadataReader),
            store: Arc::clone(&store),
        }
        .execute(&source, &ScanOptions::default())
        .unwrap();
        let plan = PlanUseCase {
            store: Arc::clone(&store),
        }
        .execute(
            &scan.scan_id,
            &PlanOptions {
                target_root: target,
                batch_size: 1,
                naming: music_folder_core::NamingRules::default(),
            },
        )
        .unwrap();
        let page = store
            .list_plan_items(&plan.plan_id, None, 1, None, Some("conflict"))
            .unwrap();
        assert_eq!(page.items.len(), 1, "only one member is loaded in the page");
        let group_id = page.items[0].conflict_group_id.as_deref().unwrap();
        let application = ApplicationState { store };
        let session = WorkflowSession::default();
        session.0.lock().unwrap().plan_id = Some(plan.plan_id.clone());

        let detail =
            query_plan_conflict_detail(&application, &session, &plan.plan_id, group_id).unwrap();
        assert_eq!(detail.members.len(), 2);
        assert!(detail
            .members
            .iter()
            .any(|member| member.source_path.ends_with("one.mp3")));
        assert!(detail
            .members
            .iter()
            .any(|member| member.source_path.ends_with("two.mp3")));
        assert_eq!(
            query_plan_conflict_detail(&application, &session, "forged-plan", group_id)
                .unwrap_err(),
            "workflow_context_stale"
        );
        assert_eq!(
            query_plan_conflict_detail(&application, &session, &plan.plan_id, "missing-group")
                .unwrap_err(),
            "plan_conflict_not_found"
        );
    }

    #[test]
    fn job_registry_is_sequenced_and_excludes_mutations() {
        let jobs = JobRegistry::default();
        let first = WorkflowSession::default();
        first.0.lock().unwrap().plan_id = Some("plan-1".into());
        let job = begin_job(&first, &jobs, 0, JobKind::Apply, true, "plan-1").unwrap();
        assert_eq!(job.event_seq, 1);
        assert!(jobs.snapshot(&job.job_id, Some(1)).unwrap().is_none());

        let second = WorkflowSession::default();
        second.0.lock().unwrap().execution_id = Some("execution-2".into());
        assert_eq!(
            begin_job(&second, &jobs, 0, JobKind::Rollback, true, "execution-2",).unwrap_err(),
            "mutation_job_busy"
        );

        let completed = Ok(WorkflowResponse {
            id: "execution-1".into(),
            success: 1,
            skipped: 0,
            failed: 0,
        });
        finish_job_session(&first, &job, &completed);
        jobs.finish(&job.job_id, completed).unwrap();
        assert_eq!(
            first.0.lock().unwrap().latest_job_id.as_deref(),
            Some(job.job_id.as_str()),
            "the terminal job remains discoverable after a WebView reload"
        );
        let snapshot = jobs.snapshot(&job.job_id, Some(1)).unwrap().unwrap();
        assert_eq!(snapshot.event_seq, 2);
        assert_eq!(snapshot.status, "completed");
        assert_eq!(snapshot.run_id.as_deref(), Some("execution-1"));
    }

    #[test]
    fn stale_plan_completion_cannot_replace_a_new_scan_context() {
        let jobs = JobRegistry::default();
        let session = WorkflowSession::default();
        session.0.lock().unwrap().scan_id = Some("scan-old".into());
        let job = begin_job(&session, &jobs, 0, JobKind::Plan, false, "scan-old").unwrap();
        {
            let mut workflow = session.0.lock().unwrap();
            workflow.workflow_generation = 2;
            workflow.active_job_id = None;
            workflow.scan_id = Some("scan-new".into());
        }
        jobs.supersede(&job.job_id).unwrap();
        let completed = Ok(WorkflowResponse {
            id: "plan-old".into(),
            success: 1,
            skipped: 0,
            failed: 0,
        });
        finish_job_session(&session, &job, &completed);
        jobs.finish(&job.job_id, completed).unwrap();
        let workflow = session.0.lock().unwrap();
        assert_eq!(workflow.scan_id.as_deref(), Some("scan-new"));
        assert!(workflow.plan_id.is_none());
        drop(workflow);
        assert_eq!(
            jobs.snapshot(&job.job_id, None).unwrap().unwrap().status,
            "cancelled"
        );
    }
}
