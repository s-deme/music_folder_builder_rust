import React, { useEffect, useReducer, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ExistingTargetConflict, PlanConflictCard } from "./conflicts";
import {
  ConfirmDialog,
  TargetEditDialog,
  type ConfirmationRequest,
  type TargetEditRequest,
} from "./dialogs";
import {
  actionLabels,
  type AttemptEvidence,
  type CleanupPreview,
  defaultNaming,
  emptyPlanCounts,
  formatReason,
  formatWorkflowError,
  type FolderSelection,
  type History,
  type HistoryArchive,
  type JobSnapshot,
  loadNaming,
  type Log,
  type Metric,
  type NamingField,
  type NamingPreview,
  type NamingRules,
  type PlanConflictDetail,
  type PlanItem,
  type PlanItemCounts,
  type PlanItemPage,
  presets,
  type Progress,
  type RecoveryItem,
  type RecoveryResult,
  riskLabels,
  type RunDetail,
  type ScanStatus,
  tokens,
  type Workflow,
  type WorkflowCapabilities,
} from "./model";
import {
  initialWorkflowContext,
  workflowReducer,
} from "./workflow";
import { RecoveryPanel } from "./recovery";
import { PathValue } from "./paths";
import "./styles.css";
import "./naming.css";

const historyKindLabels: Record<string, string> = {
  scan: "スキャン",
  plan: "整理計画",
  apply: "実行",
  verify: "検証",
  rollback: "ロールバック",
  recovery: "復旧",
  archive: "アーカイブ",
};
const historyStatusLabels: Record<string, string> = {
  running: "実行中",
  completed: "完了",
  failed: "失敗",
  cancelled: "取消済み",
  recovery_required: "復旧待ち",
  archived: "検証済みアーカイブ",
};
const jobKindLabels: Record<string, string> = {
  plan: "Plan作成",
  plan_revision: "Plan改訂",
  apply: "Apply",
  verify: "Verify",
  rollback: "Rollback",
};

const historyProtectionLabels: Record<string, string> = {
  legal_hold: "保護フラグが設定されています。管理方針を確認して保護を解除してください。",
  running_attempt: "実行中の処理があります。完了または安全な取消を待ってください。",
  recovery_required: "未完了操作があります。先に復旧パネルで状態を解決してください。",
  unrolled_mutation: "未Rollbackのファイル変更があります。対象ApplyをRollbackしてください。",
  rollback_unverified: "Rollback後のVerifyが完了していません。Rollback attemptを検証してください。",
  active_mutation_lease: "別のmutation処理がrootを使用中です。処理の完了を待ってください。",
  active_recovery: "復旧処理が実行中です。完了後に履歴を再読み込みしてください。",
  plan_build_in_progress: "Planを構築中です。完了後に履歴を再読み込みしてください。",
};
const journalStateLabels: Record<string, string> = {
  prepared: "準備済み",
  staging: "一時ファイル作成中",
  staged: "一時ファイル作成済み",
  content_verified: "内容照合済み",
  published: "移動先へ公開済み",
  source_deleted: "移動元削除済み",
  completed: "完了",
  failed: "失敗",
  recovery_required: "復旧待ち",
};
const attemptPhaseLabels: Record<string, string> = {
  apply: "Apply",
  rollback: "Rollback",
  verify: "Verify",
};

function formatHistoryProtection(reason: string) {
  return historyProtectionLabels[reason]
    ?? "安全上の依存関係により保護されています。関連する実行を開いて状態を確認してください。";
}

function historyKind(value: History) {
  if (value.kind === "apply" && value.mode === "dry_run") return "Dry-run";
  if (value.kind === "apply" && value.mode === "apply") return "本実行";
  if (value.kind === "rollback" && value.mode === "dry_run") return "Rollback dry-run";
  return historyKindLabels[value.kind] ?? value.kind;
}

function formatTime(value?: number) {
  return value === undefined || value === null
    ? "—"
    : new Intl.DateTimeFormat("ja-JP", { dateStyle: "short", timeStyle: "medium" }).format(
        new Date(value * 1000),
      );
}

function formatDuration(value: History) {
  if (!value.finished_at) return value.status === "running" ? "実行中" : "—";
  const seconds = Math.max(0, value.finished_at - value.started_at);
  return seconds < 60
    ? `${seconds}秒`
    : `${Math.floor(seconds / 60)}分${seconds % 60}秒`;
}

function NamingEditor({
  naming,
  setNaming,
  preset,
  setPreset,
  preview,
}: {
  naming: NamingRules;
  setNaming: (value: NamingRules) => void;
  preset: string;
  setPreset: (value: string) => void;
  preview?: NamingPreview;
}) {
  const [field, setField] = useState<NamingField>("filename_template");
  const [token, setToken] = useState(tokens[0]);
  const fields: [NamingField, string][] = [
    ["artist_dir_template", "アーティストフォルダ"],
    ["album_dir_template", "アルバムフォルダ"],
    ["disc_dir_template", "ディスクフォルダ"],
    ["filename_template", "音楽ファイル名"],
  ];
  const update = (key: keyof NamingRules, value: string | boolean) => {
    setPreset("custom");
    setNaming({ ...naming, [key]: value } as NamingRules);
  };

  return (
    <fieldset>
      <legend>命名設定</legend>
      <label>
        プリセット
        <select
          value={preset}
          onChange={event => {
            const value = event.target.value;
            setPreset(value);
            if (presets[value]) setNaming({ ...presets[value] });
          }}
        >
          <option value="custom">カスタム</option>
          <option value="standard">標準</option>
          <option value="flatDisc">ディスクフォルダなし</option>
          <option value="withYear">年を含める</option>
          <option value="source">元ファイル名を保持</option>
        </select>
      </label>
      {fields.map(([key, label]) => (
        <label key={key}>
          {label}
          <input
            disabled={key === "filename_template" && naming.use_source_filename}
            value={naming[key]}
            onChange={event => update(key, event.target.value)}
          />
        </label>
      ))}
      <div className="actions token-insert">
        <select value={field} onChange={event => setField(event.target.value as NamingField)}>
          {fields.map(([key, label]) => <option value={key} key={key}>{label}</option>)}
          <option value="duplicate_suffix_template">同名ファイルの末尾</option>
        </select>
        <select value={token} onChange={event => setToken(event.target.value)}>
          {tokens.map(value => <option key={value}>{value}</option>)}
        </select>
        <button type="button" onClick={() => update(field, naming[field] + token)}>項目を挿入</button>
        <button type="button" onClick={() => {
          setPreset("standard");
          setNaming({ ...defaultNaming });
        }}>既定値に戻す</button>
      </div>
      <label className="check"><input type="checkbox" checked={naming.use_source_filename} onChange={event => update("use_source_filename", event.target.checked)} />元の音楽ファイル名を使用</label>
      <label className="check"><input type="checkbox" checked={naming.use_source_image_filename} onChange={event => update("use_source_image_filename", event.target.checked)} />元の画像ファイル名を使用</label>
      <label className="check"><input type="checkbox" checked={naming.allow_missing_metadata} onChange={event => update("allow_missing_metadata", event.target.checked)} />メタデータ不足を無視してフォルダを作成する（Unknown Artist / Unknown Album）</label>
      <label className="check"><input type="checkbox" checked={naming.allow_long_paths} onChange={event => update("allow_long_paths", event.target.checked)} />Windowsの長いパスを許可する</label>
      {naming.allow_long_paths && (
        <p className="safety-warning" role="status">
          240文字を超える移動先もPlan候補にします。WindowsのLongPathsEnabled設定や保存先の制約によっては実行に失敗します。その場合も移動元は削除されません。
        </p>
      )}
      <label>
        同名ファイルの処理
        <select value={naming.duplicate_strategy} onChange={event => update("duplicate_strategy", event.target.value)}>
          <option value="skip">安全のためスキップ</option>
          <option value="sequence">安定した連番を付ける</option>
          <option value="template">カスタム末尾を付ける</option>
        </select>
      </label>
      {naming.duplicate_strategy === "template" && (
        <label>同名ファイルの末尾<input value={naming.duplicate_suffix_template} onChange={event => update("duplicate_suffix_template", event.target.value)} /></label>
      )}
      <div className="naming-preview" aria-live="polite">
        <b>生成プレビュー</b>
        <code>{preview?.relative_path ?? "確認中…"}</code>
        {preview?.issues.map((issue, index) => (
          <p className="error" key={`${issue.field}-${issue.code}-${index}`}>{issue.message}</p>
        ))}
      </div>
    </fieldset>
  );
}

const ja = {
  title: "Music Folder Builder",
  subtitle: "安全な段階型音楽ライブラリ整理",
  source: "音楽フォルダ",
  target: "整理先",
  scan: "Scan",
  plan: "Plan",
  dry: "Dry-run",
  apply: "Apply",
  verify: "Verify",
  rollback: "Rollback dry-run",
  rollbackApply: "Rollbackを実行",
  cancel: "Scanを取消",
  workflow: "ワークフロー",
  history: "実行履歴",
  logs: "実行ログ",
  planned: "整理予定",
  more: "さらに読み込む",
  refresh: "更新",
  theme: "テーマ",
  all: "すべて",
  success: "成功",
  failed: "失敗",
  skipped: "スキップ",
  conflict: "衝突",
  missing: "メタデータ不足",
  longPath: "長いパス",
  planSearch: "予定を検索",
  logSearch: "ログを検索",
  system: "システム",
  light: "ライト",
  dark: "ダーク",
};

export function App() {
  const [workflow, dispatch] = useReducer(workflowReducer, initialWorkflowContext);
  const workflowGenerationRef = useRef(0);
  const activeScanRequestRef = useRef<string>();
  const [backendReady, setBackendReady] = useState(false);
  const [workflowCapabilities, setWorkflowCapabilities] = useState<WorkflowCapabilities>();
  const [jobStarting, setJobStarting] = useState(false);
  const [sourceSelection, setSourceSelection] = useState<FolderSelection>();
  const [targetSelection, setTargetSelection] = useState<FolderSelection>();
  const [pickingFolder, setPickingFolder] = useState<"source" | "target">();
  const [naming, setNaming] = useState<NamingRules>(loadNaming);
  const [namingPreset, setNamingPreset] = useState("custom");
  const [namingPreview, setNamingPreview] = useState<NamingPreview>();
  const [history, setHistory] = useState<History[]>([]);
  const [selectedHistory, setSelectedHistory] = useState<History>();
  const [selectedHistoryPreview, setSelectedHistoryPreview] = useState<CleanupPreview>();
  const [selectedHistoryPreviewLoading, setSelectedHistoryPreviewLoading] = useState(false);
  const [attemptEvidence, setAttemptEvidence] = useState<AttemptEvidence>();
  const [attemptEvidenceLoading, setAttemptEvidenceLoading] = useState(false);
  const [historyKindFilter, setHistoryKindFilter] = useState("");
  const [historyStatusFilter, setHistoryStatusFilter] = useState("");
  const [historyQuery, setHistoryQuery] = useState("");
  const [historyOldestFirst, setHistoryOldestFirst] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyHasMore, setHistoryHasMore] = useState(false);
  const [historyUpdatedAt, setHistoryUpdatedAt] = useState<Date>();
  const [recoveries, setRecoveries] = useState<RecoveryItem[]>([]);
  const [recoveryLoading, setRecoveryLoading] = useState(false);
  const [recoveryResult, setRecoveryResult] = useState<RecoveryResult>();
  const [items, setItems] = useState<PlanItem[]>([]);
  const [planTotal, setPlanTotal] = useState(0);
  const [filteredTotal, setFilteredTotal] = useState(0);
  const [planCounts, setPlanCounts] = useState<PlanItemCounts>(emptyPlanCounts);
  const [planCursor, setPlanCursor] = useState<number | null>(null);
  const [planLoading, setPlanLoading] = useState(false);
  const [conflictDetails, setConflictDetails] = useState<Record<string, PlanConflictDetail | null>>({});
  const [logs, setLogs] = useState<Log[]>([]);
  const [metrics, setMetrics] = useState<Metric[]>([]);
  const [query, setQuery] = useState("");
  const [risk, setRisk] = useState("");
  const [logQuery, setLogQuery] = useState("");
  const [logResult, setLogResult] = useState("");
  const [error, setError] = useState<string>();
  const [confirmation, setConfirmation] = useState<ConfirmationRequest>();
  const [targetEdit, setTargetEdit] = useState<TargetEditRequest>();
  const [theme, setTheme] = useState<"system" | "light" | "dark">(
    () => (localStorage.getItem("theme") as "system" | "light" | "dark") || "system",
  );

  const planRef = useRef<HTMLDivElement>(null);
  const logRef = useRef<HTMLDivElement>(null);
  const planLoadingRef = useRef(false);
  const planRequestRef = useRef(0);
  const logRequestRef = useRef(0);
  const historyRequestRef = useRef(0);
  const namingRequestRef = useRef(0);
  const capabilityRequestRef = useRef(0);
  const historyCapabilityRequestRef = useRef(0);
  const attemptEvidenceRequestRef = useRef(0);

  const { scan, scanRequest, progress, plan, result: execution, executionId } = workflow;
  const busy = workflow.busy || jobStarting;
  const source = sourceSelection?.display ?? "";
  const target = targetSelection?.display ?? "";
  const currentCapabilities = workflowCapabilities?.workflow_generation === workflow.generation
    ? workflowCapabilities
    : undefined;
  const planCapabilities = currentCapabilities?.plan_id === plan?.id
    ? currentCapabilities
    : undefined;
  const executionCapabilities = currentCapabilities?.execution_id === executionId
    ? currentCapabilities
    : undefined;

  function nextGeneration() {
    workflowGenerationRef.current += 1;
    return workflowGenerationRef.current;
  }

  function adoptJob(job: JobSnapshot) {
    if (job.workflow_generation <= workflowGenerationRef.current) {
      throw new Error("workflow_generation_stale");
    }
    workflowGenerationRef.current = job.workflow_generation;
    localStorage.setItem("mfb.active-job", job.job_id);
    dispatch({ type: "job_started", job });
  }

  function invalidatePlanViews() {
    planRequestRef.current += 1;
    planLoadingRef.current = false;
    setPlanLoading(false);
    setItems([]);
    setPlanTotal(0);
    setFilteredTotal(0);
    setPlanCounts(emptyPlanCounts);
    setPlanCursor(null);
    setConflictDetails({});
  }

  function invalidateExecutionViews() {
    logRequestRef.current += 1;
    setLogs([]);
    setMetrics([]);
  }

  function failGeneration(generation: number, reason: unknown) {
    if (workflowGenerationRef.current !== generation) return;
    setError(formatWorkflowError(reason));
    dispatch({ type: "failed", generation });
  }

  async function chooseFolder(purpose: "source" | "target") {
    setPickingFolder(purpose);
    setError(undefined);
    try {
      const current = purpose === "source" ? sourceSelection : targetSelection;
      const selected = await invoke<FolderSelection | null>("pick_folder", {
        purpose,
        currentSelectionId: current?.selection_id ?? null,
      });
      if (!selected) return;

      activeScanRequestRef.current = undefined;
      invalidatePlanViews();
      invalidateExecutionViews();
      const generation = await invoke<number>("invalidate_workflow", {
        keepScan: purpose === "target",
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      workflowGenerationRef.current = generation;
      localStorage.removeItem("mfb.active-job");
      if (purpose === "source") {
        setSourceSelection(selected);
        dispatch({ type: "source_changed", generation });
      } else {
        setTargetSelection(selected);
        dispatch({ type: "target_changed", generation });
      }
    } catch (reason) {
      setError(formatWorkflowError(reason));
    } finally {
      setPickingFolder(undefined);
    }
  }

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("theme", theme);
  }, [theme]);

  useEffect(() => {
    let current = true;
    void invoke<boolean>("desktop_backend_ready")
      .then(ready => {
        if (!current) return;
        if (!ready) throw new Error("backend_not_ready");
        setBackendReady(ready);
      })
      .catch(reason => {
        if (current) setError(`状態DBを初期化できませんでした: ${String(reason)}`);
      });
    return () => { current = false; };
  }, []);

  useEffect(() => {
    localStorage.setItem("mfb.naming", JSON.stringify(naming));
    const request = ++namingRequestRef.current;
    const timer = window.setTimeout(() => {
      void invoke<NamingPreview>("preview_naming", { naming })
        .then(value => {
          if (request === namingRequestRef.current) setNamingPreview(value);
        })
        .catch(reason => {
          if (request === namingRequestRef.current) setError(String(reason));
        });
    }, 150);
    return () => window.clearTimeout(timer);
  }, [naming]);

  useEffect(() => {
    let offProgress: (() => void) | undefined;
    let offFinished: (() => void) | undefined;
    void listen<Progress>("scan-progress", event => {
      if (activeScanRequestRef.current !== event.payload.request_id) return;
      dispatch({
        type: "scan_progress",
        requestId: event.payload.request_id,
        progress: event.payload,
      });
    }).then(off => { offProgress = off; });
    void listen<ScanStatus>("scan-finished", event => {
      const value = event.payload;
      if (activeScanRequestRef.current !== value.request_id) return;
      dispatch({ type: "scan_finished", request: value });
      activeScanRequestRef.current = undefined;
      if (value.error) setError(value.error);
    }).then(off => { offFinished = off; });
    return () => {
      offProgress?.();
      offFinished?.();
    };
  }, []);

  useEffect(() => {
    if (!scanRequest || scanRequest.status !== "running") return;
    const generation = workflow.generation;
    const requestId = scanRequest.request_id;
    let stopped = false;
    const poll = async () => {
      try {
        const value = await invoke<ScanStatus>("scan_status", { requestId });
        if (stopped || workflowGenerationRef.current !== generation) return;
        if (value.status !== "running") {
          dispatch({ type: "scan_finished", request: value });
          activeScanRequestRef.current = undefined;
          if (value.error) setError(value.error);
        }
      } catch (reason) {
        if (
          !stopped &&
          workflowGenerationRef.current === generation &&
          String(reason) !== "scan_not_found"
        ) {
          setError(String(reason));
        }
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 500);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [scanRequest?.request_id, scanRequest?.status, workflow.generation]);

  useEffect(() => {
    if (!backendReady) return;
    let stopped = false;
    const restore = async () => {
      try {
        const active = await invoke<JobSnapshot[]>("list_active_jobs");
        const rememberedId = localStorage.getItem("mfb.active-job");
        let remembered: JobSnapshot | undefined;
        if (rememberedId) {
          try {
            remembered = (await invoke<JobSnapshot | null>("get_job_snapshot", {
              jobId: rememberedId,
              afterSeq: null,
            })) ?? undefined;
          } catch (reason) {
            if (String(reason) !== "workflow_job_not_found") throw reason;
            localStorage.removeItem("mfb.active-job");
          }
        }
        const candidates = [...active, ...(remembered ? [remembered] : [])];
        const latest = candidates.sort((left, right) =>
          right.workflow_generation - left.workflow_generation || right.event_seq - left.event_seq
        )[0];
        if (!latest || stopped || latest.workflow_generation <= workflowGenerationRef.current) return;
        workflowGenerationRef.current = latest.workflow_generation;
        localStorage.setItem("mfb.active-job", latest.job_id);
        if (latest.status === "running") {
          dispatch({ type: "job_started", job: latest });
        } else {
          dispatch({
            type: "job_started",
            job: {
              ...latest,
              event_seq: Math.max(0, latest.event_seq - 1),
              status: "running",
              result: undefined,
              error: undefined,
            },
          });
          dispatch({ type: "job_snapshot", job: latest });
          localStorage.removeItem("mfb.active-job");
        }
      } catch (reason) {
        if (!stopped && String(reason) !== "workflow_job_not_found") {
          setError(formatWorkflowError(reason));
        }
      }
    };
    void restore();
    return () => { stopped = true; };
  }, [backendReady]);

  useEffect(() => {
    const active = workflow.activeJob;
    if (!active) return;
    let stopped = false;
    const poll = async () => {
      try {
        const snapshot = await invoke<JobSnapshot | null>("get_job_snapshot", {
          jobId: active.job_id,
          afterSeq: active.event_seq,
        });
        if (
          stopped ||
          !snapshot ||
          workflowGenerationRef.current !== active.workflow_generation ||
          snapshot.job_id !== active.job_id ||
          snapshot.workflow_generation !== active.workflow_generation ||
          snapshot.event_seq <= active.event_seq
        ) return;
        if (snapshot.status !== "running") {
          if (localStorage.getItem("mfb.active-job") === snapshot.job_id) {
            localStorage.removeItem("mfb.active-job");
          }
          void loadHistory(true);
          if (snapshot.kind === "apply" || snapshot.kind === "rollback") {
            void loadRecoveries();
          }
        }
        dispatch({ type: "job_snapshot", job: snapshot });
      } catch (reason) {
        if (!stopped) setError(formatWorkflowError(reason));
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 300);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [workflow.activeJob?.job_id, workflow.activeJob?.event_seq]);

  async function startScan() {
    if (!backendReady) {
      setError("状態DBを準備しています。少し待ってから再実行してください");
      return;
    }
    activeScanRequestRef.current = undefined;
    invalidatePlanViews();
    invalidateExecutionViews();
    setError(undefined);
    setJobStarting(true);
    try {
      if (!sourceSelection) throw new Error("移動元フォルダを選択してください");
      const expectedWorkflowGeneration = workflowGenerationRef.current;
      const request = await invoke<ScanStatus>("start_scan", { sourceSelectionId: sourceSelection.selection_id, workers: null, expectedWorkflowGeneration });
      if (workflowGenerationRef.current !== expectedWorkflowGeneration) return;
      const generation = request.workflow_generation;
      workflowGenerationRef.current = generation;
      localStorage.removeItem("mfb.active-job");
      dispatch({ type: "begin_scan", generation });
      activeScanRequestRef.current = request.request_id;
      dispatch({ type: "scan_started", generation, request });
    } catch (reason) {
      setError(formatWorkflowError(reason));
    } finally {
      setJobStarting(false);
    }
  }

  async function createPlan() {
    if (!scan || !targetSelection) return;
    const scanId = scan.scan_id;
    invalidatePlanViews();
    invalidateExecutionViews();
    setError(undefined);
    setJobStarting(true);
    try {
      const value = await invoke<JobSnapshot>("create_plan", { scanId, targetSelectionId: targetSelection.selection_id, naming, expectedWorkflowGeneration: workflowGenerationRef.current });
      adoptJob(value);
    } catch (reason) {
      setError(formatWorkflowError(reason));
    } finally {
      setJobStarting(false);
    }
  }

  async function revisePlanTarget(planItemId: string, revisedTarget: string) {
    if (!plan) throw new Error("改訂元のPlanがありません");
    const planId = plan.id;
    invalidatePlanViews();
    invalidateExecutionViews();
    setError(undefined);
    setJobStarting(true);
    try {
      const authorization = await invoke<{ target_capability_id: string }>("authorize_plan_target", {
        planId,
        planItemId,
        target: revisedTarget,
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      const value = await invoke<JobSnapshot>("revise_plan_target", {
        planId,
        planItemId,
        targetCapabilityId: authorization.target_capability_id,
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      adoptJob(value);
    } catch (reason) {
      setError(formatWorkflowError(reason));
      throw new Error(formatWorkflowError(reason));
    } finally {
      setJobStarting(false);
    }
  }

  async function revisePlanConflictCandidate(
    planItemId: string,
    conflictGroupId: string,
    candidateOrdinal: number,
  ) {
    if (!plan) throw new Error("改訂元のPlanがありません");
    const planId = plan.id;
    invalidatePlanViews();
    invalidateExecutionViews();
    setError(undefined);
    setJobStarting(true);
    try {
      const authorization = await invoke<{ target_capability_id: string }>(
        "authorize_plan_conflict_candidate",
        {
          planId,
          planItemId,
          conflictGroupId,
          candidateOrdinal,
          expectedWorkflowGeneration: workflowGenerationRef.current,
        },
      );
      const value = await invoke<JobSnapshot>("revise_plan_target", {
        planId,
        planItemId,
        targetCapabilityId: authorization.target_capability_id,
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      adoptJob(value);
    } catch (reason) {
      setError(formatWorkflowError(reason));
      throw new Error(formatWorkflowError(reason));
    } finally {
      setJobStarting(false);
    }
  }

  async function executePlan(execute: boolean, confirmationSubject?: string) {
    if (!plan) throw new Error("実行するPlanがありません");
    const planId = plan.id;
    invalidateExecutionViews();
    setError(undefined);
    setJobStarting(true);
    try {
      const value = await invoke<JobSnapshot>("apply_plan", {
        planId,
        execute,
        confirm: execute ? confirmationSubject ?? null : null,
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      adoptJob(value);
    } catch (reason) {
      setError(formatWorkflowError(reason));
      throw new Error(formatWorkflowError(reason));
    } finally {
      setJobStarting(false);
    }
  }

  async function executeOperation(
    kind: "verify" | "rollback",
    execute = false,
    confirmationSubject?: string,
  ) {
    if (!executionId) throw new Error("対象の実行がありません");
    const currentExecutionId = executionId;
    setError(undefined);
    setJobStarting(true);
    try {
      const value = kind === "verify"
        ? await invoke<JobSnapshot>("verify_execution", { executionId: currentExecutionId, expectedWorkflowGeneration: workflowGenerationRef.current })
        : await invoke<JobSnapshot>("rollback_execution", {
            executionId: currentExecutionId,
            execute,
            confirm: execute ? confirmationSubject ?? null : null,
            expectedWorkflowGeneration: workflowGenerationRef.current,
          });
      adoptJob(value);
    } catch (reason) {
      setError(formatWorkflowError(reason));
      throw new Error(formatWorkflowError(reason));
    } finally {
      setJobStarting(false);
    }
  }

  async function loadRecoveries() {
    if (!backendReady) return;
    setRecoveryLoading(true);
    try {
      const values = await invoke<RecoveryItem[]>("list_recovery_items");
      setRecoveries(values);
    } catch (reason) {
      setError(formatWorkflowError(reason));
    } finally {
      setRecoveryLoading(false);
    }
  }

  async function performRecovery(item: RecoveryItem, execute: boolean) {
    setRecoveryLoading(true);
    setError(undefined);
    try {
      const value = await invoke<RecoveryResult>("run_recovery", {
        operationId: item.operation_id,
        action: item.suggested_action,
        execute,
        confirm: execute ? item.operation_id : null,
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      workflowGenerationRef.current = value.workflow_generation;
      setRecoveryResult(value);
      await loadRecoveries();
      await loadHistory(true);
      if (execute && value.verify_failed > 0) {
        setError(`復旧後Verifyで${value.verify_failed}件の不一致を検出しました。新しい操作を開始せず、診断を確認してください。`);
      }
    } catch (reason) {
      setError(formatWorkflowError(reason));
      await loadRecoveries();
      throw new Error(formatWorkflowError(reason));
    } finally {
      setRecoveryLoading(false);
    }
  }

  function requestRecoveryExecution(item: RecoveryItem) {
    setConfirmation({
      title: "復旧操作を実行しますか？",
      message: `operation ${item.operation_id} に ${item.suggested_action} を実行します。元: ${item.source.display} / 移動先: ${item.target.display}。事前照合に失敗した場合は何も変更しません。`,
      confirmLabel: "復旧を実行",
      danger: true,
      onConfirm: () => performRecovery(item, true),
    });
  }

  async function loadItems(reset = false) {
    if (!plan || (!reset && (planCursor === null || planLoadingRef.current))) return;
    const generation = workflow.generation;
    const planId = plan.id;
    const request = reset ? ++planRequestRef.current : planRequestRef.current;
    planLoadingRef.current = true;
    setPlanLoading(true);
    try {
      const page = await invoke<PlanItemPage>("list_plan_items", {
        planId,
        cursor: reset ? undefined : planCursor,
        limit: 200,
        query: query || null,
        risk: risk || null,
      });
      if (
        request !== planRequestRef.current ||
        generation !== workflowGenerationRef.current
      ) return;
      setItems(previous => {
        const candidates = reset ? page.items : [...previous, ...page.items];
        const unique = Array.from(new Map(candidates.map(item => [item.id, item])).values());
        return unique.slice(0, page.filtered_total);
      });
      setPlanTotal(page.total);
      setFilteredTotal(page.filtered_total);
      setPlanCounts(page.counts);
      setPlanCursor(page.next_cursor);
      if (reset) planRef.current?.scrollTo({ top: 0 });
    } catch (reason) {
      if (request === planRequestRef.current && generation === workflowGenerationRef.current) {
        setError(String(reason));
      }
    } finally {
      if (request === planRequestRef.current && generation === workflowGenerationRef.current) {
        planLoadingRef.current = false;
        setPlanLoading(false);
      }
    }
  }

  async function toggleConflict(value: PlanItem) {
    const groupId = value.conflict_group_id;
    if (!plan || !groupId) return;
    if (groupId in conflictDetails) {
      setConflictDetails(current => {
        const next = { ...current };
        delete next[groupId];
        return next;
      });
      return;
    }
    const generation = workflow.generation;
    const planId = plan.id;
    setConflictDetails(current => ({ ...current, [groupId]: null }));
    try {
      const detail = await invoke<PlanConflictDetail>("get_plan_conflict_detail", {
        planId,
        conflictGroupId: groupId,
      });
      if (generation === workflowGenerationRef.current) {
        setConflictDetails(current => ({ ...current, [groupId]: detail }));
      }
    } catch (reason) {
      if (generation !== workflowGenerationRef.current) return;
      setConflictDetails(current => {
        const next = { ...current };
        delete next[groupId];
        return next;
      });
      setError(String(reason));
    }
  }

  async function loadLogs(reset = false) {
    if (!executionId) return;
    const generation = workflow.generation;
    const currentExecutionId = executionId;
    const request = reset ? ++logRequestRef.current : logRequestRef.current;
    try {
      const rows = await invoke<Log[]>("list_operation_logs", {
        executionId: currentExecutionId,
        cursor: reset ? undefined : logs.at(-1)?.sequence_no,
        limit: 200,
        query: logQuery || null,
        result: logResult || null,
      });
      if (request !== logRequestRef.current || generation !== workflowGenerationRef.current) return;
      setLogs(previous => reset ? rows : [...previous, ...rows]);
      if (reset) {
        logRef.current?.scrollTo({ top: 0 });
        const values = await invoke<Metric[]>("list_metrics", { runId: currentExecutionId });
        if (request === logRequestRef.current && generation === workflowGenerationRef.current) {
          setMetrics(values);
        }
      }
    } catch (reason) {
      if (request === logRequestRef.current && generation === workflowGenerationRef.current) {
        setError(String(reason));
      }
    }
  }

  async function loadHistory(reset = false) {
    if (!backendReady) return;
    const request = reset ? ++historyRequestRef.current : historyRequestRef.current;
    setHistoryLoading(true);
    setError(undefined);
    try {
      const cursor = reset ? undefined : history.at(-1);
      const rows = await invoke<History[]>("list_history", {
        limit: 100,
        cursorStartedAt: cursor?.started_at,
        cursorId: cursor?.id,
        kind: historyKindFilter || null,
        status: historyStatusFilter || null,
        query: historyQuery.trim() || null,
        oldestFirst: historyOldestFirst,
      });
      if (request !== historyRequestRef.current) return;
      setHistory(previous => reset ? rows : [...previous, ...rows]);
      setHistoryHasMore(rows.length === 100);
      setHistoryUpdatedAt(new Date());
      if (reset) {
        setSelectedHistory(current =>
          current && rows.some(row => row.id === current.id) ? current : undefined,
        );
      }
    } catch (reason) {
      if (request === historyRequestRef.current) setError(String(reason));
    } finally {
      if (request === historyRequestRef.current) setHistoryLoading(false);
    }
  }

  async function restoreHistory(value: History) {
    let generation = workflowGenerationRef.current;
    invalidatePlanViews();
    invalidateExecutionViews();
    setError(undefined);
    try {
      generation = await invoke<number>("activate_history", {
        kind: value.kind,
        runId: value.id,
        expectedWorkflowGeneration: workflowGenerationRef.current,
      });
      workflowGenerationRef.current = generation;
      localStorage.removeItem("mfb.active-job");
      dispatch({ type: "begin_restore", generation });
      const detail = await invoke<RunDetail>("get_run_detail", { kind: value.kind, runId: value.id });
      const result = {
        id: detail.id,
        success: detail.success,
        skipped: detail.skipped,
        failed: detail.failed,
      };
      if (workflowGenerationRef.current !== generation) return;
      if (detail.kind === "scan") {
        dispatch({
          type: "restore_scan",
          generation,
          scan: {
            scan_id: detail.id,
            files: detail.success,
            cache_hits: 0,
            warnings: detail.failed,
          },
        });
      } else if (detail.kind === "plan") {
        dispatch({ type: "restore_plan", generation, plan: result });
      } else {
        const restoredExecutionId = detail.kind === "apply" ? detail.id : detail.parent_id;
        if (!restoredExecutionId) throw new Error("親executionを特定できません");
        dispatch({
          type: "restore_execution",
          generation,
          result,
          executionId: restoredExecutionId,
        });
      }
      const values = await invoke<Metric[]>("list_metrics", { runId: detail.id });
      if (workflowGenerationRef.current === generation) setMetrics(values);
    } catch (reason) {
      failGeneration(generation, reason);
    }
  }

  async function requestHistoryArchive() {
    if (!selectedHistory) return;
    try {
      const selected = selectedHistory;
      const preview = await invoke<CleanupPreview>("history_cleanup_preview", {
        kind: selected.kind,
        runId: selected.id,
      });
      if (preview.blocked) {
        setError(`保護中の履歴は整理できません: ${preview.protection_reasons.join("、")}`);
        return;
      }
      setConfirmation({
        title: "履歴を検証付きで整理しますか？",
        message: `${historyKind(selected)}をJSONLへアーカイブします。Plan ${preview.plans}件、execution ${preview.executions}件、log ${preview.logs}件、推定 ${preview.estimated_bytes.toLocaleString()} bytesが対象です。保存先: ${preview.archive_path.display}。実ファイルと履歴DBのrecordは削除しません。`,
        confirmLabel: "アーカイブを作成",
        danger: false,
        onConfirm: async () => {
          const archived = await invoke<HistoryArchive>("archive_history", {
            kind: selected.kind,
            runId: selected.id,
            confirm: preview.root_scan_id,
            expectedWorkflowGeneration: workflowGenerationRef.current,
          });
          if (!archived.verified) throw new Error("アーカイブのdigestを検証できませんでした");
          const generation = nextGeneration();
          localStorage.removeItem("mfb.active-job");
          dispatch({ type: "source_changed", generation });
          invalidatePlanViews();
          invalidateExecutionViews();
          setSelectedHistory(undefined);
          await loadHistory(true);
        },
      });
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function requestHistoryPurge() {
    if (!selectedHistory) return;
    try {
      const selected = selectedHistory;
      const preview = await invoke<CleanupPreview>("history_cleanup_preview", {
        kind: selected.kind,
        runId: selected.id,
      });
      if (preview.blocked) {
        setError(`保護中の履歴は削除できません: ${preview.protection_reasons.join("、")}`);
        return;
      }
      if (!preview.archived) {
        setError("履歴を削除する前に、検証済みアーカイブを作成してください。");
        return;
      }
      setConfirmation({
        title: "検証済み履歴をデータベースから削除しますか？",
        message: `ワークフロー ${preview.root_scan_id} のPlan ${preview.plans}件、execution ${preview.executions}件、log ${preview.logs}件をactive DBから削除します。検証済みJSONLアーカイブは ${preview.archive_path.display} に残り、音楽ファイルは削除しません。復元にはそのアーカイブが必要です。`,
        confirmLabel: "履歴を完全削除",
        danger: true,
        onConfirm: async () => {
          await invoke("delete_history", {
            kind: "scan",
            runId: preview.root_scan_id,
            confirm: preview.root_scan_id,
            expectedWorkflowGeneration: workflowGenerationRef.current,
          });
          const generation = nextGeneration();
          localStorage.removeItem("mfb.active-job");
          dispatch({ type: "source_changed", generation });
          invalidatePlanViews();
          invalidateExecutionViews();
          setSelectedHistory(undefined);
          await loadHistory(true);
        },
      });
    } catch (reason) {
      setError(formatWorkflowError(reason));
    }
  }

  useEffect(() => {
    if (plan) void loadItems(true);
  }, [plan?.id, query, risk]);
  useEffect(() => {
    if (executionId) void loadLogs(true);
  }, [executionId, logQuery, logResult]);
  useEffect(() => {
    if (!backendReady) return;
    const timer = window.setTimeout(() => void loadHistory(true), 200);
    return () => window.clearTimeout(timer);
  }, [backendReady, historyKindFilter, historyStatusFilter, historyQuery, historyOldestFirst]);
  useEffect(() => {
    if (backendReady) void loadRecoveries();
  }, [backendReady]);
  useEffect(() => {
    if (!backendReady) return;
    const request = ++capabilityRequestRef.current;
    void invoke<WorkflowCapabilities>("get_workflow_capabilities")
      .then(value => {
        if (request !== capabilityRequestRef.current) return;
        if (value.workflow_generation !== workflowGenerationRef.current) {
          setWorkflowCapabilities(undefined);
          return;
        }
        setWorkflowCapabilities(value);
      })
      .catch(reason => {
        if (request !== capabilityRequestRef.current) return;
        setWorkflowCapabilities(undefined);
        setError(formatWorkflowError(reason));
      });
  }, [
    backendReady,
    workflow.generation,
    workflow.busy,
    jobStarting,
    plan?.id,
    executionId,
    recoveries.length,
  ]);
  useEffect(() => {
    const selected = selectedHistory;
    const request = ++historyCapabilityRequestRef.current;
    setSelectedHistoryPreview(undefined);
    if (!backendReady || !selected || selected.kind === "archive") {
      setSelectedHistoryPreviewLoading(false);
      return;
    }
    setSelectedHistoryPreviewLoading(true);
    void invoke<CleanupPreview>("history_cleanup_preview", {
      kind: selected.kind,
      runId: selected.id,
    })
      .then(preview => {
        if (request === historyCapabilityRequestRef.current) {
          setSelectedHistoryPreview(preview);
        }
      })
      .catch(reason => {
        if (request === historyCapabilityRequestRef.current) {
          setError(formatWorkflowError(reason));
        }
      })
      .finally(() => {
        if (request === historyCapabilityRequestRef.current) {
          setSelectedHistoryPreviewLoading(false);
        }
      });
  }, [backendReady, selectedHistory?.kind, selectedHistory?.id, workflow.generation]);
  useEffect(() => {
    const selected = selectedHistory;
    const request = ++attemptEvidenceRequestRef.current;
    setAttemptEvidence(undefined);
    if (!backendReady || !selected) {
      setAttemptEvidenceLoading(false);
      return;
    }
    setAttemptEvidenceLoading(true);
    void invoke<AttemptEvidence>("list_attempt_evidence", {
      kind: selected.kind,
      runId: selected.id,
    })
      .then(value => {
        if (request === attemptEvidenceRequestRef.current) setAttemptEvidence(value);
      })
      .catch(reason => {
        if (request === attemptEvidenceRequestRef.current) setError(formatWorkflowError(reason));
      })
      .finally(() => {
        if (request === attemptEvidenceRequestRef.current) setAttemptEvidenceLoading(false);
      });
  }, [backendReady, selectedHistory?.kind, selectedHistory?.id]);

  const historyGroups = Array.from(
    history.reduce((groups, row) => {
      const rows = groups.get(row.root_scan_id) ?? [];
      rows.push(row);
      groups.set(row.root_scan_id, rows);
      return groups;
    }, new Map<string, History[]>()),
  );
  const workflowStatus = {
    idle: "Scan未実行",
    scanning: "Scan実行中",
    scan_ready: "Scan完了・Plan作成待ち",
    planning: "Plan作成中",
    plan_ready: "Plan作成済み",
    executing: "処理実行中",
    execution_ready: "実行結果を確認できます",
  }[workflow.phase];

  return (
    <main>
      <header className="app-header">
        <div>
          <h1>{ja.title}</h1>
          <p>{ja.subtitle}</p>
          <small className="backend-status">
            {backendReady ? "状態DB準備完了" : "状態DB準備中…"}
          </small>
        </div>
        <label className="theme-select">
          {ja.theme}
          <select value={theme} onChange={event => setTheme(event.target.value as typeof theme)}>
            <option value="system">{ja.system}</option>
            <option value="light">{ja.light}</option>
            <option value="dark">{ja.dark}</option>
          </select>
        </label>
      </header>

      {(recoveryLoading || recoveries.length > 0 || recoveryResult) && (
        <RecoveryPanel
          loading={recoveryLoading}
          items={recoveries}
          result={recoveryResult}
          actionsAllowed={currentCapabilities?.recovery_execute.allowed === true}
          onRefresh={() => void loadRecoveries()}
          onDryRun={item => void performRecovery(item, false).catch(() => undefined)}
          onExecute={requestRecoveryExecution}
        />
      )}

      <section aria-labelledby="workflow-title">
        <h2 id="workflow-title">{ja.workflow}</h2>
        <div className="folder-field">
          <label htmlFor="source-folder">{ja.source}</label>
          <div className="folder-picker">
            <input id="source-folder" value={source} readOnly placeholder="フォルダを選択してください" />
            <button type="button" className="secondary" disabled={!backendReady || busy || pickingFolder !== undefined} onClick={() => void chooseFolder("source")}>
              {pickingFolder === "source" ? "選択中…" : "参照…"}
            </button>
          </div>
        </div>
        <div className="folder-field">
          <label htmlFor="target-folder">{ja.target}</label>
          <div className="folder-picker">
            <input id="target-folder" value={target} readOnly placeholder="フォルダを選択してください" />
            <button type="button" className="secondary" disabled={!backendReady || busy || pickingFolder !== undefined} onClick={() => void chooseFolder("target")}>
              {pickingFolder === "target" ? "選択中…" : "参照…"}
            </button>
          </div>
        </div>
        <NamingEditor naming={naming} setNaming={setNaming} preset={namingPreset} setPreset={setNamingPreset} preview={namingPreview} />
        <div className="workflow-status" aria-live="polite"><span>現在の状態</span><strong>{workflowStatus}</strong></div>
        <div className="actions">
          <button disabled={!backendReady || !source || busy} onClick={() => void startScan()}>{ja.scan}</button>
          <button disabled={!scanRequest || scanRequest.status !== "running"} onClick={() => scanRequest && void invoke("cancel_scan", { scanId: scanRequest.request_id })}>{ja.cancel}</button>
          <button disabled={!scan || !target || busy || !namingPreview || namingPreview.issues.length > 0} onClick={() => void createPlan()}>{ja.plan}</button>
          <button
            disabled={busy || planCapabilities?.apply_dry_run.allowed !== true}
            onClick={() => void executePlan(false).catch(() => undefined)}
          >{ja.dry}</button>
          <button
            className="danger"
            disabled={busy || planCapabilities?.apply_execute.allowed !== true}
            onClick={() => {
              const confirmationSubject = planCapabilities?.apply_execute.confirmation_subject;
              if (!confirmationSubject) return;
              setConfirmation({
              title: "本実行しますか？",
              message: `保存済みPlan ${confirmationSubject} のsnapshotを検証し、ファイルを移動します。`,
              confirmLabel: "Applyを実行",
              danger: true,
              onConfirm: () => executePlan(true, confirmationSubject),
              });
            }}
          >{ja.apply}</button>
          <button
            disabled={busy || executionCapabilities?.verify.allowed !== true}
            onClick={() => void executeOperation("verify").catch(() => undefined)}
          >{ja.verify}</button>
          <button
            disabled={busy || executionCapabilities?.rollback_dry_run.allowed !== true}
            onClick={() => void executeOperation("rollback", false).catch(() => undefined)}
          >{ja.rollback}</button>
          <button
            className="danger"
            disabled={busy || executionCapabilities?.rollback_execute.allowed !== true}
            onClick={() => {
              const confirmationSubject = executionCapabilities?.rollback_execute.confirmation_subject;
              if (!confirmationSubject) return;
              setConfirmation({
              title: "Rollbackを実行しますか？",
              message: `実行 ${confirmationSubject} を巻き戻します。移動先ファイルの削除を伴う場合があります。`,
              confirmLabel: "Rollbackを実行",
              danger: true,
              onConfirm: () => executeOperation("rollback", true, confirmationSubject),
              });
            }}
          >{ja.rollbackApply}</button>
        </div>
        {currentCapabilities && (
          <div className="capability-status" aria-live="polite">
            {!currentCapabilities.apply_execute.allowed && plan && (
              <small>Apply保護中: {currentCapabilities.apply_execute.protection_reasons.map(formatWorkflowError).join(" ")}</small>
            )}
            {!currentCapabilities.rollback_execute.allowed && executionId && (
              <small>Rollback保護中: {currentCapabilities.rollback_execute.protection_reasons.map(formatWorkflowError).join(" ")}</small>
            )}
          </div>
        )}
        <div aria-live="polite">
          {scanRequest && <output>Scan状態: {scanRequest.status}</output>}
          {progress && <output>{progress.phase}: {progress.processed}/{progress.enumerated}件 / {progress.items_per_second.toFixed(1)}件/秒 / ETA {progress.eta_seconds ?? "-"}秒 / cache {progress.cache_hits} / 警告 {progress.warnings}</output>}
          {scan && <output>Scan: {scan.files}件 / cache {scan.cache_hits} / 警告 {scan.warnings}</output>}
          {plan && <output>Plan ID {plan.id}: {plan.success}件 / conflict {plan.skipped} / risk {plan.failed}</output>}
          {execution && <output>実行: success {execution.success} / skip {execution.skipped} / fail {execution.failed}</output>}
        </div>
        {workflow.lastJob && (
          <p
            className={workflow.lastJob.status === "failed" ? "error job-outcome" : "job-outcome"}
            role={workflow.lastJob.status === "failed" ? "alert" : "status"}
            data-job-status={workflow.lastJob.status}
          >
            {jobKindLabels[workflow.lastJob.kind] ?? workflow.lastJob.kind}
            {workflow.lastJob.status === "completed" && "が完了しました。"}
            {workflow.lastJob.status === "cancelled" && "は取り消されました。"}
            {workflow.lastJob.status === "failed" && `に失敗しました: ${formatWorkflowError(workflow.lastJob.error ?? "詳細不明")}`}
          </p>
        )}
        {error && <p className="error" role="alert">{error}</p>}
      </section>

      <section aria-labelledby="logs-title">
        <h2 id="logs-title">{ja.logs}</h2>
        <input aria-label={ja.logSearch} placeholder={ja.logSearch} value={logQuery} onChange={event => setLogQuery(event.target.value)} />
        <select aria-label="ログ結果で絞り込み" value={logResult} onChange={event => setLogResult(event.target.value)}>
          <option value="">{ja.all}</option>
          <option value="success">{ja.success}</option>
          <option value="failed">{ja.failed}</option>
          <option value="skipped">{ja.skipped}</option>
        </select>
        <div className="virtual log-list" ref={logRef}>
            {logs.map((value, index) => {
              const targetConflict = value.error === "target_already_exists";
              return (
                <div
                  className={`row${targetConflict ? " conflict-row" : ""}`}
                  data-index={index}
                  key={value.id}
                >
                  <b>#{value.sequence_no}</b><span>{value.result}</span>
                  {targetConflict
                    ? <ExistingTargetConflict log={value} />
                    : <>
                        <span>{value.action}</span>
                        <div className="path-line"><span>元</span><PathValue path={value.source} fallback={value.source_path} copy /></div>
                        <div className="path-line"><span>移動先</span><PathValue path={value.target} fallback={value.target_path ?? "—"} copy={value.target !== undefined || value.target_path !== undefined} /></div>
                        {value.error && <small>{formatReason(value.error)}</small>}
                      </>}
                </div>
              );
            })}
        </div>
        <button disabled={!executionId} onClick={() => void loadLogs()}>{ja.more}</button>
        {metrics.map(value => <output key={`${value.phase}-${value.elapsed_ms}`}>{value.phase}: {value.elapsed_ms}ms / {value.item_count}件</output>)}
      </section>

      <section aria-labelledby="plan-title">
        <div className="section-title"><h2 id="plan-title">{ja.planned}</h2>{plan && <strong>{planTotal.toLocaleString()}件</strong>}</div>
        {plan && (
          <div className="plan-summary" aria-label="Plan件数">
            <div><span>移動</span><b>{planCounts.moves.toLocaleString()}</b></div>
            <div><span>スキップ</span><b>{planCounts.skips.toLocaleString()}</b></div>
            <div className={planCounts.needs_attention > 0 ? "attention" : ""}><span>要確認</span><b>{planCounts.needs_attention.toLocaleString()}</b></div>
          </div>
        )}
        <div className="plan-tools">
          <input aria-label={ja.planSearch} placeholder={ja.planSearch} value={query} onChange={event => setQuery(event.target.value)} />
          <select aria-label="リスクで絞り込み" value={risk} onChange={event => setRisk(event.target.value)}>
            <option value="">{ja.all} ({(planCounts.moves + planCounts.skips).toLocaleString()})</option>
            <option value="conflict">{ja.conflict} ({planCounts.conflicts.toLocaleString()})</option>
            <option value="invalid_target">無効な移動先 ({planCounts.invalid_target.toLocaleString()})</option>
            <option value="metadata_missing">{ja.missing} ({planCounts.metadata_missing.toLocaleString()})</option>
            <option value="path_too_long">{ja.longPath} ({planCounts.path_too_long.toLocaleString()})</option>
          </select>
        </div>
        <div className="virtual plan-list" ref={planRef}>
            {items.map((value, index) => {
              const groupId = value.conflict_group_id;
              const detail = groupId ? conflictDetails[groupId] : undefined;
              const imageConflict = value.reason === "companion_target_ambiguous";
              return (
                <article
                  className="plan-item"
                  data-index={index}
                  key={value.id}
                >
                  <header>
                    <b>#{value.ordinal}</b>
                    <span className="badge">{actionLabels[value.action] ?? value.action}</span>
                    {value.risk !== "none" && <span className="badge risk">{riskLabels[value.risk] ?? value.risk}</span>}
                  </header>
                  <div className="path-line"><span>元</span><PathValue path={value.source} fallback={value.source_path} copy /></div>
                  <div className="path-line"><span>移動先</span><PathValue path={value.target} fallback={value.target_path ?? (imageConflict ? `未決定（候補${value.conflict_member_count}件）` : "—")} copy={value.target !== undefined || value.target_path !== undefined} /></div>
                  {value.reason && <div className="reason"><span>理由</span><span>{formatReason(value.reason)}</span></div>}
                  {groupId && !imageConflict && plan && <PlanConflictCard planId={plan.id} item={value} />}
                  {groupId && imageConflict && groupId in conflictDetails && (
                    <div className="conflict-detail">
                      {detail ? <>
                        <strong>移動先候補（{detail.candidates.length}件）</strong>
                        {detail.candidates.map(candidate => (
                          <div className="conflict-candidate" key={`${groupId}:${candidate.ordinal}`}>
                            <div className="path-line"><span>候補</span><PathValue path={candidate.target} fallback={candidate.target_path} copy /></div>
                            <strong>対応する音楽（{candidate.members.length}件）</strong>
                            {candidate.members.map(member => (
                              <div className="conflict-member" key={member.item_id}>
                                <span>#{member.ordinal}</span><PathValue path={member.source} fallback={member.source_path} copy />
                              </div>
                            ))}
                            <button type="button" disabled={!plan || busy} onClick={() => void revisePlanConflictCandidate(value.id, groupId, candidate.ordinal).catch(() => undefined)}>この移動先を選択</button>
                          </div>
                        ))}
                      </> : <span>候補を読み込んでいます…</span>}
                    </div>
                  )}
                  <footer>
                    {groupId && imageConflict && <button type="button" className="secondary" onClick={() => void toggleConflict(value)}>{groupId in conflictDetails ? "移動先候補を閉じる" : `移動先候補を確認（${value.conflict_member_count}件）`}</button>}
                    <button
                      type="button"
                      className="secondary"
                      disabled={!plan || busy}
                      onClick={() => setTargetEdit({
                        currentTarget: "",
                        onSubmit: revisedTarget => revisePlanTarget(value.id, revisedTarget),
                      })}
                    >移動先を変更</button>
                  </footer>
                </article>
              );
            })}
        </div>
        {plan && (
          <div className="plan-pagination">
            <span>{items.length.toLocaleString()} / {filteredTotal.toLocaleString()}件を表示{filteredTotal !== planTotal && `（全${planTotal.toLocaleString()}件）`}</span>
            {planCursor !== null
              ? <button disabled={busy || planLoading} onClick={() => void loadItems()}>{planLoading ? "読み込み中…" : `${ja.more}（次の${Math.min(200, Math.max(0, filteredTotal - items.length)).toLocaleString()}件）`}</button>
              : items.length > 0 && <span>すべて読み込みました</span>}
          </div>
        )}
      </section>

      <section className="history-section" aria-labelledby="history-title">
        <div className="section-title">
          <h2 id="history-title">{ja.history}</h2>
          <div className="history-refresh">
            <small>{historyUpdatedAt ? `最終更新 ${historyUpdatedAt.toLocaleTimeString("ja-JP")}` : "未更新"}</small>
            <button className="secondary" disabled={!backendReady || historyLoading} onClick={() => void loadHistory(true)}>{historyLoading ? "更新中…" : ja.refresh}</button>
          </div>
        </div>
        <div className="history-tools">
          <input aria-label="実行IDを検索" placeholder="実行IDを検索" value={historyQuery} onChange={event => setHistoryQuery(event.target.value)} />
          <select aria-label="種別で絞り込み" value={historyKindFilter} onChange={event => setHistoryKindFilter(event.target.value)}>
            <option value="">すべての種別</option><option value="scan">スキャン</option><option value="plan">整理計画</option><option value="apply">Dry-run・実行</option><option value="verify">検証</option><option value="rollback">ロールバック</option><option value="recovery">復旧</option><option value="archive">アーカイブ</option>
          </select>
          <select aria-label="状態で絞り込み" value={historyStatusFilter} onChange={event => setHistoryStatusFilter(event.target.value)}>
            <option value="">すべての状態</option><option value="running">実行中</option><option value="completed">完了</option><option value="failed">失敗</option><option value="cancelled">取消済み</option><option value="recovery_required">復旧待ち</option><option value="archived">検証済みアーカイブ</option>
          </select>
          <select aria-label="並び順" value={historyOldestFirst ? "oldest" : "newest"} onChange={event => setHistoryOldestFirst(event.target.value === "oldest")}>
            <option value="newest">新しい順</option><option value="oldest">古い順</option>
          </select>
        </div>
        {historyLoading && history.length === 0
          ? <p className="empty-state">履歴を読み込んでいます…</p>
          : history.length === 0
            ? <p className="empty-state">条件に一致する実行履歴はありません。</p>
            : (
              <div className="history-layout">
                <div className="history-list" role="table" aria-label="実行履歴">
                  <div className="history-header" role="row"><span>実行日時</span><span>種別</span><span>状態</span><span>結果</span><span>ID</span></div>
                  {historyGroups.map(([rootId, rows]) => (
                    <div className="history-group" key={rootId}>
                      <div className="history-group-title">ワークフロー <code title={rootId}>{rootId.slice(0, 8)}</code></div>
                      {rows.map(value => (
                        <button
                          type="button"
                          aria-pressed={selectedHistory?.id === value.id}
                          className={`history-entry${selectedHistory?.id === value.id ? " selected" : ""}`}
                          key={`${value.kind}-${value.id}`}
                          onClick={() => setSelectedHistory(value)}
                        >
                          <time>{formatTime(value.started_at)}</time><strong>{historyKind(value)} <small>試行 #{value.attempt_no}</small></strong>
                          <span className={`status-badge status-${value.status}`}>{historyStatusLabels[value.status] ?? value.status}</span>
                          <span className="history-result">成功 {value.success.toLocaleString()}<small>スキップ {value.skipped.toLocaleString()} / {value.kind === "scan" ? "警告" : "失敗"} {value.failed.toLocaleString()}</small></span>
                          <code title={value.id}>{value.id.slice(0, 8)}</code>
                        </button>
                      ))}
                    </div>
                  ))}
                </div>
                <aside className="history-detail" aria-label="実行履歴の詳細">
                  {selectedHistory ? <>
                    <div><span className={`status-badge status-${selectedHistory.status}`}>{historyStatusLabels[selectedHistory.status] ?? selectedHistory.status}</span><h3>{historyKind(selectedHistory)}</h3></div>
                    <dl>
                      <dt>開始</dt><dd>{formatTime(selectedHistory.started_at)}</dd><dt>終了</dt><dd>{formatTime(selectedHistory.finished_at)}</dd><dt>所要時間</dt><dd>{formatDuration(selectedHistory)}</dd><dt>試行</dt><dd>#{selectedHistory.attempt_no}</dd><dt>実行主体</dt><dd>{selectedHistory.actor}</dd><dt>成功</dt><dd>{selectedHistory.success.toLocaleString()}件</dd><dt>スキップ</dt><dd>{selectedHistory.skipped.toLocaleString()}件</dd><dt>{selectedHistory.kind === "scan" ? "警告" : "失敗"}</dt><dd>{selectedHistory.failed.toLocaleString()}件</dd>
                      {selectedHistory.parent_id && <><dt>親run</dt><dd><code>{selectedHistory.parent_id}</code></dd></>}
                      {selectedHistory.predecessor_attempt_id && <><dt>前回の試行</dt><dd><code>{selectedHistory.predecessor_attempt_id}</code></dd></>}
                      {selectedHistory.error_code && <><dt>エラー</dt><dd>{formatWorkflowError(selectedHistory.error_code)}</dd></>}
                    </dl>
                    <div className="history-full-id"><span>実行ID</span><code>{selectedHistory.id}</code><button className="secondary compact" onClick={() => void navigator.clipboard.writeText(selectedHistory.id)}>コピー</button></div>
                    <div className="history-full-id"><span>論理run ID</span><code>{selectedHistory.logical_run_id}</code><button className="secondary compact" onClick={() => void navigator.clipboard.writeText(selectedHistory.logical_run_id)}>コピー</button></div>
                    {attemptEvidenceLoading && <p className="empty-state">この試行の記録を読み込んでいます…</p>}
                    {attemptEvidence && (
                      <div className="attempt-evidence" aria-label="この試行の記録">
                        {attemptEvidence.preflights.length > 0 && <div>
                          <h4>事前照合</h4>
                          {attemptEvidence.preflights.map(value => <p key={value.id}>{attemptPhaseLabels[value.kind] ?? "事前照合"}（{value.mode === "dry_run" ? "Dry-run" : "実行"}）: 通過 {value.passed} / 中止 {value.blocked}</p>)}
                        </div>}
                        {attemptEvidence.journal.length > 0 && <div>
                          <h4>操作 journal</h4>
                          {attemptEvidence.journal.map(value => <p key={value.id}>#{value.sequence_no} {attemptPhaseLabels[value.direction] ?? value.direction}: {journalStateLabels[value.state] ?? "状態不明"}{value.error && ` — ${formatWorkflowError(value.error)}`}</p>)}
                        </div>}
                        {attemptEvidence.logs.length > 0 && <div>
                          <h4>試行ログ</h4>
                          {attemptEvidence.logs.map((value, index) => <p key={`${value.phase}-${value.sequence_no}-${index}`}>#{value.sequence_no} {attemptPhaseLabels[value.phase] ?? "操作"}: {value.result === "success" ? "成功" : value.result === "skipped" ? "スキップ" : "失敗"}{value.error && ` — ${formatWorkflowError(value.error)}`}</p>)}
                        </div>}
                        {attemptEvidence.preflights.length === 0 && attemptEvidence.journal.length === 0 && attemptEvidence.logs.length === 0 && <p>この試行に追加の記録はありません。</p>}
                      </div>
                    )}
                    {selectedHistoryPreviewLoading && <p className="empty-state">保護状態を確認しています…</p>}
                    {selectedHistoryPreview?.blocked && (
                      <div className="history-protection" role="status">
                        <strong>この履歴は保護中です</strong>
                        <ul>{selectedHistoryPreview.protection_reasons.map(reason => <li key={reason}>{formatHistoryProtection(reason)}</li>)}</ul>
                      </div>
                    )}
                    <div className="history-detail-actions">
                      <button disabled={selectedHistory.kind === "archive" || selectedHistory.kind === "recovery"} onClick={() => void restoreHistory(selectedHistory)}>この実行を開く</button>
                      <button
                        className="secondary"
                        disabled={
                          busy
                          || !selectedHistoryPreview
                          || selectedHistoryPreview.blocked
                          || selectedHistoryPreview.archived
                        }
                        onClick={() => void requestHistoryArchive()}
                      >履歴を整理</button>
                      <button
                        className="danger"
                        disabled={
                          busy
                          || !selectedHistoryPreview
                          || selectedHistoryPreview.blocked
                          || !selectedHistoryPreview.archived
                        }
                        onClick={() => void requestHistoryPurge()}
                      >検証済み履歴を削除</button>
                    </div>
                  </> : <p className="empty-state">履歴を選択すると詳細を表示します。</p>}
                </aside>
              </div>
            )}
        <div className="history-footer">
          {history.length > 0 && <span>{history.length.toLocaleString()}件を表示</span>}
          {historyHasMore
            ? <button className="secondary" disabled={historyLoading} onClick={() => void loadHistory()}>{historyLoading ? "読込中…" : ja.more}</button>
            : history.length > 0 && <span>すべて読み込みました</span>}
        </div>
      </section>

      <ConfirmDialog request={confirmation} onClose={() => setConfirmation(undefined)} />
      <TargetEditDialog request={targetEdit} onClose={() => setTargetEdit(undefined)} />
    </main>
  );
}

if (typeof document !== "undefined") {
  const rootElement = document.getElementById("root");
  if (rootElement) {
    createRoot(rootElement).render(
      <React.StrictMode><App /></React.StrictMode>,
    );
  }
}
