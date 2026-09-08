export type Scan = { scan_id: string; files: number; cache_hits: number; warnings: number };
export type FolderSelection = { selection_id: string; display: string };
export type ScanStatus = { request_id: string; workflow_generation: number; status: string; scan_id?: string; files: number; cache_hits: number; warnings: number; error?: string };
export type Progress = { request_id: string; workflow_generation: number; scan_id: string; phase: string; enumerated: number; processed: number; cache_hits: number; warnings: number; elapsed_ms: number; items_per_second: number; eta_seconds?: number };
export type Workflow = { id: string; success: number; skipped: number; failed: number };
export type WorkflowActionCapability = { allowed: boolean; protection_reasons: string[]; confirmation_subject?: string };
export type WorkflowCapabilities = {
  workflow_generation: number;
  scan_id?: string;
  plan_id?: string;
  execution_id?: string;
  recovery_count: number;
  apply_dry_run: WorkflowActionCapability;
  apply_execute: WorkflowActionCapability;
  verify: WorkflowActionCapability;
  rollback_dry_run: WorkflowActionCapability;
  rollback_execute: WorkflowActionCapability;
  recovery_execute: WorkflowActionCapability;
};
export type JobKind = "plan" | "plan_revision" | "apply" | "verify" | "rollback";
export type JobSnapshot = { job_id: string; workflow_generation: number; event_seq: number; kind: JobKind; phase: string; status: "running" | "completed" | "failed" | "cancelled"; input_id: string; run_id?: string; scan_id?: string; plan_id?: string; execution_id?: string; result?: Workflow; error?: string };
export type History = { id: string; kind: string; mode?: string; logical_run_id: string; attempt_no: number; predecessor_attempt_id?: string; actor: string; status: string; started_at: number; finished_at?: number; parent_id?: string; root_scan_id: string; success: number; skipped: number; failed: number; error_code?: string };
export type RunDetail = { id: string; kind: string; logical_run_id: string; attempt_no: number; predecessor_attempt_id?: string; actor: string; status: string; parent_id?: string; success: number; skipped: number; failed: number; error_code?: string };
export type AttemptEvidence = {
  preflights: { id: string; kind: string; mode: string; status: string; passed: number; blocked: number }[];
  journal: { id: string; sequence_no: number; direction: string; state: string; error?: string }[];
  logs: { sequence_no: number; phase: string; result: string; error?: string }[];
};
export type PlanItem = { id: string; conflict_group_id?: string; conflict_member_count: number; ordinal: number; source_path: string; source?: LosslessPathEnvelope; target_path?: string; target?: LosslessPathEnvelope; action: string; risk: string; reason?: string };
export type PlanItemCounts = { moves: number; skips: number; needs_attention: number; conflicts: number; invalid_target: number; metadata_missing: number; path_too_long: number };
export type PlanItemPage = { items: PlanItem[]; total: number; filtered_total: number; next_cursor: number | null; counts: PlanItemCounts };
export type ConflictMember = { item_id: string; ordinal: number; source_path: string; source?: LosslessPathEnvelope };
export type PlanConflictDetail = { id: string; kind: string; target_path: string; target?: LosslessPathEnvelope; existing_target_path?: string; existing_target?: LosslessPathEnvelope; members: ConflictMember[]; candidates: { ordinal: number; target_path: string; target?: LosslessPathEnvelope; members: ConflictMember[] }[] };
export type Log = { id: string; execution_id: string; sequence_no: number; source_path: string; source?: LosslessPathEnvelope; target_path?: string; target?: LosslessPathEnvelope; action: string; result: string; error?: string; created_at: number };
export type Metric = { phase: string; elapsed_ms: number; item_count: number };
export type DuplicateStrategy = "skip" | "sequence" | "template";
export type NamingRules = { artist_dir_template: string; album_dir_template: string; disc_dir_template: string; filename_template: string; duplicate_suffix_template: string; use_source_filename: boolean; use_source_image_filename: boolean; allow_missing_metadata: boolean; allow_long_paths: boolean; duplicate_strategy: DuplicateStrategy };
export type NamingField = "artist_dir_template" | "album_dir_template" | "disc_dir_template" | "filename_template" | "duplicate_suffix_template";
export type NamingPreview = { relative_path: string; issues: { field: string; code: string; message: string }[] };
export type LosslessPathEnvelope = { schema_version: number; role: string; display: string; display_lossy: boolean; encoding: string; raw_base64: string };
export type CleanupPreview = { root_scan_id: string; plans: number; executions: number; logs: number; blocked: boolean; protection_reasons: string[]; estimated_bytes: number; archive_path: LosslessPathEnvelope; archived: boolean };
export type HistoryArchive = { archive_id: string; root_scan_id: string; archive_path: LosslessPathEnvelope; schema_version: number; record_count: number; byte_count: number; digest_algorithm: string; sha256: string; verified: boolean; already_archived: boolean };
export type RecoveryAction = "resume" | "rollback_published" | "discard_unpublished_temporary" | "manual_intervention";
export type RecoveryItem = { operation_id: string; attempt_id: string; sequence_no: number; state: string; source: LosslessPathEnvelope; target: LosslessPathEnvelope; temporary: LosslessPathEnvelope; expected_size?: number; suggested_action: RecoveryAction };
export type RecoveryResult = { recovery_id: string; operation_id: string; action: RecoveryAction; dry_run: boolean; verify_id?: string; verify_success: number; verify_failed: number; workflow_generation: number };

export const defaultNaming: NamingRules = { artist_dir_template: "{album_artist}", album_dir_template: "{album}", disc_dir_template: "[{disc_no:02d}]", filename_template: "[{track_no:02d}_]{title}{extension}", duplicate_suffix_template: "_{disc_no:02d}", use_source_filename: false, use_source_image_filename: false, allow_missing_metadata: false, allow_long_paths: false, duplicate_strategy: "skip" };
export const presets: Record<string, NamingRules> = { standard: defaultNaming, flatDisc: { ...defaultNaming, disc_dir_template: "" }, withYear: { ...defaultNaming, album_dir_template: "[{year} - ]{album}" }, source: { ...defaultNaming, use_source_filename: true, use_source_image_filename: true } };
export const tokens = ["{album_artist}", "{artist}", "{album}", "{title}", "{track_no:02d}", "{disc_no:02d}", "{year}", "{source_stem}", "{extension}"];
export const emptyPlanCounts: PlanItemCounts = { moves: 0, skips: 0, needs_attention: 0, conflicts: 0, invalid_target: 0, metadata_missing: 0, path_too_long: 0 };
export const riskLabels: Record<string, string> = { none: "問題なし", conflict: "衝突", invalid_target: "無効な移動先", metadata_missing: "メタデータ不足", path_too_long: "長いパス" };
export const actionLabels: Record<string, string> = { move: "移動", skip: "スキップ" };

const reasonLabels: Record<string, string> = {
  empty_path: "移動先のパスが空です", metadata_missing: "メタデータを読み取れません", artist_missing: "アーティスト情報がありません", album_missing: "アルバム情報がありません", artist_album_missing: "アーティスト情報とアルバム情報がありません", source_equals_target: "移動元と移動先が同じです", target_conflict: "同じ移動先になるファイルがあります", companion_without_music: "対応する音楽ファイルがありません", companion_target_ambiguous: "画像の移動先を一意に決められません", image_pending_anchor: "画像に対応する音楽ファイルを確認しています", manual_target: "移動先が手動で変更されました", already_applied_for_plan: "この整理計画はすでに実行済みです", source_or_target_missing: "移動元または移動先がありません", target_already_exists: "移動先にファイルがすでに存在します", target_missing_in_log: "実行ログに移動先がありません", target_missing: "移動先がありません", target_changed_since_apply: "実行後に移動先が変更されたため巻き戻しません", source_already_exists: "移動元にファイルがすでに存在します", reverse_target_delete_failed: "巻き戻し時に移動先を削除できませんでした", partial_copy_cleanup_failed: "部分的にコピーされた移動先を削除できませんでした",
};

export function formatReason(reason: string): string {
  const [code, actual, limit] = reason.split(":");
  if (code === "path_too_long" && actual && limit) return `パス全体が長すぎます: ${actual}文字（上限${limit}文字）`;
  if (code === "component_too_long" && actual && limit) return `フォルダ名またはファイル名が長すぎます: ${actual}文字（上限${limit}文字）`;
  if (reasonLabels[reason]) return reasonLabels[reason];
  return /^[a-z][a-z0-9_]*(?::\d+)*$/.test(reason) ? "詳細不明の理由があります" : reason;
}

const workflowErrorLabels: Record<string, string> = {
  plan_snapshot_mismatch: "保存されたPlanの整合性を確認できませんでした。安全のため実行を中止しました。Planを作り直してください。",
  plan_snapshot_missing: "Planの整合性情報がありません。安全のため実行を中止しました。Planを作り直してください。",
  plan_not_completed: "完了していないPlanは実行できません。",
  recovery_required: "未完了のファイル操作があります。新しいApply/Rollbackより先に復旧パネルを確認してください。",
  recovery_context_stale: "復旧対象が更新されました。一覧を再読み込みしてください。",
  recovery_confirmation_required: "本復旧には対象operation IDとの確認一致が必要です。",
  manual_intervention_required: "自動的に安全な状態を一意に決められません。診断をexportし、元・一時・移動先を手動確認してください。",
  workflow_generation_stale: "画面のワークフロー状態が更新されています。最新の状態を読み直してください。",
  workflow_context_superseded: "この処理は新しいワークフローに置き換えられました。",
  workflow_job_busy: "別のワークフロー処理が実行中です。完了後に再実行してください。",
  mutation_job_busy: "ファイルを変更する処理が実行中です。完了または復旧後に再実行してください。",
  scan_job_busy: "スキャンが実行中です。完了または取消後に再実行してください。",
  operation_confirmation_required: "本実行には対象IDとの確認一致が必要です。画面を読み直してから再実行してください。",
  history_archive_confirmation_required: "履歴アーカイブの確認対象が更新されました。履歴を読み直してください。",
  history_purge_confirmation_required: "履歴削除の確認対象が一致しません。履歴を読み直してください。",
  stage_disk_full: "保存先の空き容量が不足したため、コピーを完了できませんでした。元ファイルは保持されています。空き容量を確保してから再試行してください。",
  stage_permission_denied: "保存先へ書き込む権限がありません。元ファイルは保持されています。アクセス権と他のアプリによる使用状況を確認してください。",
  atomic_publish_permission_denied: "移動先へ公開する権限がありません。元ファイルは保持されています。アクセス権を確認してから再試行してください。",
  workflow_archived_read_only: "整理済みの履歴は変更できません。現在のワークフローから新しい操作を開始してください。",
};

type WorkflowErrorPayload = { code?: unknown; correlation_id?: unknown };

function workflowErrorParts(reason: unknown): { code: string; correlationId?: string } {
  if (typeof reason === "object" && reason !== null) {
    const payload = reason as WorkflowErrorPayload;
    return {
      code: typeof payload.code === "string" ? payload.code : "diagnostic_unknown",
      correlationId: typeof payload.correlation_id === "string" ? payload.correlation_id : undefined,
    };
  }
  return { code: String(reason) };
}

export function formatWorkflowError(reason: unknown): string {
  const { code, correlationId } = workflowErrorParts(reason);
  const stableCode = code.split(":", 1)[0];
  const known = workflowErrorLabels[code] ?? workflowErrorLabels[stableCode];
  if (known) return correlationId ? `${known} 問い合わせID: ${correlationId}` : known;
  let fallback = "詳細不明のエラーが発生しました。安全のため操作を繰り返さず、診断履歴を確認してください。";
  if (/disk_full|no_space/i.test(code)) fallback = "保存先の空き容量が不足しています。元ファイルを保持したまま、空き容量を確保して再試行してください。";
  else if (/permission|access_denied/i.test(code)) fallback = "ファイルへのアクセスが拒否されました。元ファイルを保持したまま、アクセス権を確認してください。";
  else if (/path_too_long|filename_too_long/i.test(code)) fallback = "パスが長すぎるため処理できませんでした。元ファイルは保持されています。Windowsの長いパス設定と移動先を確認してください。";
  else if (/reparse|outside_root|path_traversal/i.test(code)) fallback = "安全でないパス操作を拒否しました。元ファイルを保持したまま、移動元と移動先を確認してください。";
  else if (/published_|recovery|journal_transition/i.test(code)) fallback = "ファイル操作の途中状態が記録されています。新しい操作をせず、復旧パネルを確認してください。";
  return correlationId ? `${fallback} 問い合わせID: ${correlationId}` : `${fallback} 問い合わせIDは診断履歴で確認できます。`;
}

export function sourceFileName(path: string) { return path.split(/[\\/]/).at(-1) ?? "image"; }
export function joinPath(directory: string, filename: string) { return `${directory.replace(/[\\/]$/, "")}\\${filename}`; }
export function loadNaming(): NamingRules { try { return { ...defaultNaming, ...JSON.parse(localStorage.getItem("mfb.naming") ?? "{}") as Partial<NamingRules> }; } catch { return defaultNaming; } }
