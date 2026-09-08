import {
  initialWorkflowContext,
  workflowReducer,
  type WorkflowContext,
} from "./workflow";

function expect(value: boolean, message: string): void {
  if (!value) throw new Error(`workflow reducer fixture: ${message}`);
}

export function exerciseWorkflowReducer(): WorkflowContext {
  const scanning = workflowReducer(initialWorkflowContext, {
    type: "begin_scan",
    generation: 1,
  });
  const requested = workflowReducer(scanning, {
    type: "scan_started",
    generation: 1,
    request: {
      request_id: "scan-request",
      workflow_generation: 1,
      status: "running",
      files: 0,
      cache_hits: 0,
      warnings: 0,
    },
  });
  const staleFinish = workflowReducer(requested, {
    type: "scan_finished",
    request: {
      request_id: "old-request",
      workflow_generation: 1,
      status: "completed",
      scan_id: "old-scan",
      files: 1,
      cache_hits: 0,
      warnings: 0,
    },
  });
  expect(staleFinish === requested, "stale scan completion must be ignored");

  const completed = workflowReducer(requested, {
    type: "scan_finished",
    request: {
      request_id: "scan-request",
      workflow_generation: 1,
      status: "completed",
      scan_id: "scan-1",
      files: 2,
      cache_hits: 1,
      warnings: 0,
    },
  });
  const planning = workflowReducer(completed, { type: "begin_plan", generation: 2 });
  const planned = workflowReducer(planning, {
    type: "plan_ready",
    generation: 2,
    plan: { id: "plan-1", success: 2, skipped: 0, failed: 0 },
  });
  const executing = workflowReducer(planned, {
    type: "begin_execution",
    generation: 3,
  });
  const executed = workflowReducer(executing, {
    type: "execution_ready",
    generation: 3,
    executionId: "execution-1",
    result: { id: "execution-1", success: 2, skipped: 0, failed: 0 },
  });
  const replanning = workflowReducer(executed, { type: "begin_plan", generation: 4 });
  expect(!replanning.plan, "new plan must invalidate the old plan");
  expect(!replanning.executionId, "new plan must invalidate the old execution");

  const staleExecution = workflowReducer(replanning, {
    type: "execution_ready",
    generation: 3,
    executionId: "stale-execution",
    result: { id: "stale-execution", success: 1, skipped: 0, failed: 0 },
  });
  expect(staleExecution === replanning, "stale generation response must be ignored");
  const sourceChanged = workflowReducer(executed, {
    type: "source_changed",
    generation: 4,
  });
  expect(!sourceChanged.scan, "source change must invalidate the old scan");
  expect(!sourceChanged.executionId, "source change must invalidate the old execution");

  const targetChanged = workflowReducer(executed, {
    type: "target_changed",
    generation: 4,
  });
  expect(targetChanged.scan?.scan_id === "scan-1", "target change must retain the scan");
  expect(!targetChanged.plan, "target change must invalidate the old plan");
  expect(!targetChanged.executionId, "target change must invalidate the old execution");

  const restoring = workflowReducer(staleExecution, { type: "begin_restore", generation: 5 });
  const restored = workflowReducer(restoring, {
    type: "restore_execution",
    generation: 5,
    executionId: "restored-execution",
    result: { id: "restored-execution", success: 2, skipped: 0, failed: 0 },
  });
  expect(restored.executionId === "restored-execution", "history restore must settle its generation");
  return restored;
}

export const workflowReducerFixtureResult = exerciseWorkflowReducer();
