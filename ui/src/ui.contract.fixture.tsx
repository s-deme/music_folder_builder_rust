import { renderToStaticMarkup } from "react-dom/server";
import { ExistingTargetConflict, PlanConflictCard, requestConflictDetail } from "./conflicts";
import {
  cancelModal,
  ConfirmDialog,
  openModalAndFocus,
  performDialogAction,
  returnFocus,
  TargetEditDialog,
} from "./dialogs";
import type { JobSnapshot, Log, PlanItem, RecoveryItem } from "./model";
import { PathValue } from "./paths";
import { RecoveryPanel } from "./recovery";
import {
  exerciseWorkflowReducer,
  workflowReducerFixtureResult,
} from "./workflow.reducer.fixture";
import { workflowReducer, type WorkflowContext } from "./workflow";

export { App } from "./main";
export { ExistingTargetConflict, PlanConflictCard } from "./conflicts";
export { ConfirmDialog, TargetEditDialog } from "./dialogs";
export { losslessClipboardValue } from "./paths";

export function renderLosslessPathContract() {
  return renderToStaticMarkup(
    <PathValue
      path={{
        schema_version: 1,
        role: "plan_source",
        display: "C:\\Incoming\\表示�.flac",
        display_lossy: true,
        encoding: "windows_utf16le_v1",
        raw_base64: "QwA6AFwAANg=",
      }}
      copy
    />,
  );
}

export function reducerStaleGenerationContract() {
  const exercised = exerciseWorkflowReducer();
  const settled: WorkflowContext = {
    generation: 9,
    phase: "execution_ready",
    busy: false,
    executionId: "current-execution",
    result: { id: "current-execution", success: 1, skipped: 0, failed: 0 },
  };
  const afterStaleExecution = workflowReducer(settled, {
    type: "execution_ready",
    generation: 8,
    executionId: "stale-execution",
    result: { id: "stale-execution", success: 99, skipped: 0, failed: 0 },
  });
  const afterStaleFailure = workflowReducer(settled, {
    type: "failed",
    generation: 8,
  });
  const scanning = workflowReducer({ generation: 10, phase: "scanning", busy: true }, {
    type: "scan_started",
    generation: 10,
    request: { request_id: "active", workflow_generation: 10, status: "running", files: 0, cache_hits: 0, warnings: 0 },
  });
  const staleProgress = workflowReducer(scanning, {
    type: "scan_progress",
    requestId: "stale",
    progress: { request_id: "stale", workflow_generation: 10, scan_id: "scan-stale", phase: "metadata", enumerated: 4, processed: 3, cache_hits: 0, warnings: 0, elapsed_ms: 1, items_per_second: 1 },
  });
  const cancelled = workflowReducer(scanning, {
    type: "scan_finished",
    request: { request_id: "active", workflow_generation: 10, status: "cancelled", files: 2, cache_hits: 0, warnings: 0 },
  });

  return {
    fixtureExecutionId: workflowReducerFixtureResult.executionId,
    exercisedExecutionId: exercised.executionId,
    staleExecutionIgnored: afterStaleExecution === settled,
    staleFailureIgnored: afterStaleFailure === settled,
    staleProgressIgnored: staleProgress === scanning,
    cancellationSettled: cancelled.phase === "idle" && !cancelled.busy && !cancelled.scan,
  };
}

export function jobSnapshotReducerContract() {
  const running: JobSnapshot = {
    job_id: "job-plan",
    workflow_generation: 6,
    event_seq: 1,
    kind: "plan",
    phase: "planning",
    status: "running",
    input_id: "scan-1",
    scan_id: "scan-1",
  };
  const started = workflowReducer({
    generation: 5,
    phase: "scan_ready",
    busy: false,
    scan: { scan_id: "scan-1", files: 2, cache_hits: 0, warnings: 0 },
  }, { type: "job_started", job: running });
  const wrongJob = workflowReducer(started, {
    type: "job_snapshot",
    job: { ...running, job_id: "job-other", event_seq: 2 },
  });
  const staleGeneration = workflowReducer(started, {
    type: "job_snapshot",
    job: { ...running, workflow_generation: 5, event_seq: 2 },
  });
  const duplicate = workflowReducer(started, {
    type: "job_snapshot",
    job: { ...running, result: { id: "forged", success: 9, skipped: 0, failed: 0 } },
  });
  const completedSnapshot: JobSnapshot = {
    ...running,
    event_seq: 2,
    status: "completed",
    run_id: "plan-1",
    result: { id: "plan-1", success: 2, skipped: 0, failed: 0 },
  };
  const completed = workflowReducer(started, { type: "job_snapshot", job: completedSnapshot });
  const delayed = workflowReducer(completed, {
    type: "job_snapshot",
    job: { ...completedSnapshot, event_seq: 3, result: { id: "late-plan", success: 9, skipped: 0, failed: 0 } },
  });

  const reloadTerminal: JobSnapshot = {
    job_id: "job-apply",
    workflow_generation: 12,
    event_seq: 2,
    kind: "apply",
    phase: "applying",
    status: "completed",
    input_id: "plan-reload",
    plan_id: "plan-reload",
    run_id: "execution-reload",
    result: { id: "execution-reload", success: 2, skipped: 0, failed: 0 },
  };
  const reloaded = workflowReducer({ generation: 0, phase: "idle", busy: false }, {
    type: "job_started",
    job: { ...reloadTerminal, event_seq: 1, status: "running", result: undefined, run_id: undefined },
  });
  const reloadCompleted = workflowReducer(reloaded, { type: "job_snapshot", job: reloadTerminal });

  return {
    wrongJobIgnored: wrongJob === started,
    staleGenerationIgnored: staleGeneration === started,
    duplicateIgnored: duplicate === started,
    completedPlanId: completed.plan?.id,
    lateTerminalIgnored: delayed === completed,
    reloadExecutionId: reloadCompleted.executionId,
    reloadSettled: !reloadCompleted.busy && !reloadCompleted.activeJob,
  };
}

export function renderDialogContracts() {
  const confirm = renderToStaticMarkup(
    <ConfirmDialog
      request={{
        title: "履歴を削除",
        message: "この操作は取り消せません。",
        confirmLabel: "確認して削除",
        danger: true,
        onConfirm: () => undefined,
      }}
      onClose={() => undefined}
    />,
  );
  const targetEdit = renderToStaticMarkup(
    <TargetEditDialog
      request={{
        currentTarget: "C:\\Music\\Artist\\Album\\track.mp3",
        onSubmit: () => undefined,
      }}
      onClose={() => undefined}
    />,
  );
  return { confirm, targetEdit };
}

export function renderConflictContracts() {
  const item: PlanItem = {
    id: "item-current",
    conflict_group_id: "conflict-group",
    conflict_member_count: 2,
    ordinal: 1,
    source_path: "C:\\Incoming\\current.mp3",
    target_path: "C:\\Music\\Artist\\Album\\track.mp3",
    action: "skip",
    risk: "conflict",
    reason: "target_conflict",
  };
  const log: Log = {
    id: "operation-1",
    execution_id: "execution-1",
    sequence_no: 1,
    source_path: "C:\\Incoming\\current.mp3",
    target_path: "C:\\Music\\Artist\\Album\\existing.mp3",
    action: "copy_source_retained",
    result: "failed",
    error: "target_already_exists",
    created_at: 1,
  };

  return {
    planConflict: renderToStaticMarkup(
      <PlanConflictCard planId="plan-1" item={item} />,
    ),
    existingTarget: renderToStaticMarkup(<ExistingTargetConflict log={log} />),
  };
}

export function renderRecoveryContract() {
  const recovery: RecoveryItem = {
    operation_id: "operation-recovery-1",
    attempt_id: "execution-crashed-1",
    sequence_no: 7,
    state: "content_verified",
    source: { schema_version: 1, role: "recovery_source", display: "C:\\Incoming\\track.mp3", display_lossy: false, encoding: "windows_utf16le_v1", raw_base64: "source" },
    temporary: { schema_version: 1, role: "recovery_temporary", display: "C:\\Music\\.mfb-operation-recovery-1.staging", display_lossy: false, encoding: "windows_utf16le_v1", raw_base64: "temporary" },
    target: { schema_version: 1, role: "recovery_target", display: "C:\\Music\\Artist\\track.mp3", display_lossy: false, encoding: "windows_utf16le_v1", raw_base64: "target" },
    expected_size: 1234,
    suggested_action: "resume",
  };
  return renderToStaticMarkup(
    <RecoveryPanel
      loading={false}
      actionsAllowed
      items={[recovery]}
      result={{
        recovery_id: "recovery-1",
        operation_id: recovery.operation_id,
        action: "resume",
        dry_run: true,
        verify_success: 0,
        verify_failed: 0,
        workflow_generation: 11,
      }}
      onRefresh={() => undefined}
      onDryRun={() => undefined}
      onExecute={() => undefined}
    />,
  );
}

export async function dialogRuntimeContract() {
  let modalOpened = 0;
  let initialFocus = 0;
  const dialog = { open: false, showModal: () => { modalOpened += 1; dialog.open = true; } };
  openModalAndFocus(dialog, { focus: () => { initialFocus += 1; } });

  let prevented = 0;
  let closed = 0;
  cancelModal({ preventDefault: () => { prevented += 1; } }, () => { closed += 1; });

  let returnedFocus = 0;
  let scheduled = 0;
  returnFocus(
    { focus: () => { returnedFocus += 1; } },
    callback => { scheduled += 1; callback(); },
  );

  let failures = 0;
  let successes = 0;
  await performDialogAction(
    async () => { throw new Error("deterministic failure"); },
    () => { successes += 1; },
    () => { failures += 1; },
  );
  await performDialogAction(
    async () => undefined,
    () => { successes += 1; },
    () => { failures += 1; },
  );
  return { modalOpened, initialFocus, prevented, closed, scheduled, returnedFocus, failures, successes };
}

export async function conflictCommandMockContract() {
  let calls = 0;
  const failure = requestConflictDetail(async () => {
    calls += 1;
    throw new Error("backend unavailable");
  }, "plan-1", "group-1").then(
    () => false,
    () => true,
  );
  const failed = await failure;
  const recovered = await requestConflictDetail(async (command, arguments_) => {
    calls += 1;
    if (command !== "get_plan_conflict_detail" || arguments_.planId !== "plan-1") {
      throw new Error("unexpected command contract");
    }
    return {
      id: arguments_.conflictGroupId,
      kind: "same_target",
      target_path: "C:\\Music\\track.mp3",
      members: [],
      candidates: [],
    };
  }, "plan-1", "group-1");
  return { calls, failed, recoveredId: recovered.id };
}
