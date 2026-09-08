import type { JobSnapshot, Progress, Scan, ScanStatus, Workflow } from "./model";

export type WorkflowPhase =
  | "idle"
  | "scanning"
  | "scan_ready"
  | "planning"
  | "plan_ready"
  | "executing"
  | "execution_ready";

export type WorkflowContext = {
  generation: number;
  phase: WorkflowPhase;
  busy: boolean;
  scan?: Scan;
  scanRequest?: ScanStatus;
  progress?: Progress;
  plan?: Workflow;
  result?: Workflow;
  executionId?: string;
  activeJob?: JobSnapshot;
  lastJob?: JobSnapshot;
};

export type WorkflowAction =
  | { type: "begin_scan"; generation: number }
  | { type: "scan_started"; generation: number; request: ScanStatus }
  | { type: "scan_progress"; requestId: string; progress: Progress }
  | { type: "scan_finished"; request: ScanStatus }
  | { type: "begin_plan"; generation: number }
  | { type: "plan_ready"; generation: number; plan: Workflow }
  | { type: "begin_execution"; generation: number }
  | { type: "execution_ready"; generation: number; result: Workflow; executionId: string }
  | { type: "begin_operation"; generation: number }
  | { type: "operation_ready"; generation: number; result: Workflow }
  | { type: "job_started"; job: JobSnapshot }
  | { type: "job_snapshot"; job: JobSnapshot }
  | { type: "begin_restore"; generation: number }
  | { type: "source_changed"; generation: number }
  | { type: "target_changed"; generation: number }
  | { type: "restore_scan"; generation: number; scan: Scan }
  | { type: "restore_plan"; generation: number; plan: Workflow }
  | { type: "restore_execution"; generation: number; result: Workflow; executionId: string }
  | { type: "failed"; generation: number };

export const initialWorkflowContext: WorkflowContext = {
  generation: 0,
  phase: "idle",
  busy: false,
};

function settledPhase(state: WorkflowContext): WorkflowPhase {
  if (state.executionId || state.result) return "execution_ready";
  if (state.plan) return "plan_ready";
  if (state.scan) return "scan_ready";
  return "idle";
}

function current(state: WorkflowContext, generation: number): boolean {
  return generation === state.generation;
}

export function workflowReducer(
  state: WorkflowContext,
  action: WorkflowAction,
): WorkflowContext {
  switch (action.type) {
    case "source_changed":
      if (action.generation <= state.generation) return state;
      return { generation: action.generation, phase: "idle", busy: false };
    case "target_changed":
      if (action.generation <= state.generation) return state;
      return {
        generation: action.generation,
        phase: state.scan ? "scan_ready" : "idle",
        busy: false,
        scan: state.scan,
        scanRequest: state.scanRequest,
      };
    case "begin_scan":
      if (action.generation <= state.generation) return state;
      return { generation: action.generation, phase: "scanning", busy: true };
    case "scan_started":
      if (
        !current(state, action.generation) ||
        action.request.workflow_generation !== action.generation
      ) return state;
      return { ...state, phase: "scanning", busy: true, scanRequest: action.request };
    case "scan_progress":
      if (
        state.phase !== "scanning" ||
        state.scanRequest?.request_id !== action.requestId ||
        action.progress.workflow_generation !== state.generation
      ) {
        return state;
      }
      return { ...state, progress: action.progress };
    case "scan_finished": {
      if (
        state.scanRequest?.request_id !== action.request.request_id ||
        action.request.workflow_generation !== state.generation
      ) return state;
      const completed = action.request.status === "completed" && action.request.scan_id;
      return {
        ...state,
        phase: completed ? "scan_ready" : "idle",
        busy: false,
        scanRequest: action.request,
        progress: undefined,
        scan: completed
          ? {
              scan_id: action.request.scan_id!,
              files: action.request.files,
              cache_hits: action.request.cache_hits,
              warnings: action.request.warnings,
            }
          : undefined,
      };
    }
    case "begin_plan":
      if (action.generation <= state.generation) return state;
      return {
        generation: action.generation,
        phase: "planning",
        busy: true,
        scan: state.scan,
      };
    case "plan_ready":
      if (!current(state, action.generation)) return state;
      return { ...state, phase: "plan_ready", busy: false, plan: action.plan };
    case "begin_execution":
      if (action.generation <= state.generation) return state;
      return {
        generation: action.generation,
        phase: "executing",
        busy: true,
        scan: state.scan,
        plan: state.plan,
      };
    case "execution_ready":
      if (!current(state, action.generation)) return state;
      return {
        ...state,
        phase: "execution_ready",
        busy: false,
        result: action.result,
        executionId: action.executionId,
      };
    case "begin_operation":
      if (action.generation <= state.generation) return state;
      return {
        ...state,
        generation: action.generation,
        phase: "executing",
        busy: true,
      };
    case "operation_ready":
      if (!current(state, action.generation)) return state;
      return {
        ...state,
        phase: "execution_ready",
        busy: false,
        result: action.result,
      };
    case "job_started": {
      if (
        action.job.workflow_generation <= state.generation ||
        action.job.status !== "running" ||
        action.job.event_seq < 1
      ) return state;
      const scanId = action.job.scan_id;
      const planId = action.job.plan_id;
      const executionId = action.job.execution_id;
      const phase = action.job.kind === "plan" || action.job.kind === "plan_revision"
        ? "planning"
        : "executing";
      return {
        generation: action.job.workflow_generation,
        phase,
        busy: true,
        scan: state.scan ?? (scanId ? { scan_id: scanId, files: 0, cache_hits: 0, warnings: 0 } : undefined),
        plan: state.plan ?? (planId ? { id: planId, success: 0, skipped: 0, failed: 0 } : undefined),
        result: state.result,
        executionId: state.executionId ?? executionId,
        activeJob: action.job,
      };
    }
    case "job_snapshot": {
      const active = state.activeJob;
      if (
        !active ||
        active.job_id !== action.job.job_id ||
        active.workflow_generation !== action.job.workflow_generation ||
        state.generation !== action.job.workflow_generation ||
        action.job.event_seq <= active.event_seq
      ) return state;
      if (action.job.status === "running") {
        return { ...state, activeJob: action.job };
      }
      if (action.job.status !== "completed" || !action.job.result) {
        return {
          ...state,
          phase: settledPhase(state),
          busy: false,
          activeJob: undefined,
          lastJob: action.job,
        };
      }
      if (action.job.kind === "plan" || action.job.kind === "plan_revision") {
        return {
          ...state,
          phase: "plan_ready",
          busy: false,
          plan: action.job.result,
          result: undefined,
          executionId: undefined,
          activeJob: undefined,
          lastJob: action.job,
        };
      }
      if (action.job.kind === "apply") {
        return {
          ...state,
          phase: "execution_ready",
          busy: false,
          result: action.job.result,
          executionId: action.job.result.id,
          activeJob: undefined,
          lastJob: action.job,
        };
      }
      return {
        ...state,
        phase: "execution_ready",
        busy: false,
        result: action.job.result,
        activeJob: undefined,
        lastJob: action.job,
      };
    }
    case "begin_restore":
      if (action.generation <= state.generation) return state;
      return { generation: action.generation, phase: "idle", busy: true };
    case "restore_scan":
      if (!current(state, action.generation)) return state;
      return {
        generation: action.generation,
        phase: "scan_ready",
        busy: false,
        scan: action.scan,
      };
    case "restore_plan":
      if (!current(state, action.generation)) return state;
      return {
        generation: action.generation,
        phase: "plan_ready",
        busy: false,
        plan: action.plan,
      };
    case "restore_execution":
      if (!current(state, action.generation)) return state;
      return {
        generation: action.generation,
        phase: "execution_ready",
        busy: false,
        result: action.result,
        executionId: action.executionId,
      };
    case "failed":
      if (!current(state, action.generation)) return state;
      return { ...state, phase: settledPhase(state), busy: false };
  }
}
