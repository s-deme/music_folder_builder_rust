export type Scan = { scan_id: string; files: number; cache_hits: number; warnings: number };
export type ScanStatus = { request_id: string; status: string; scan_id?: string; files: number; cache_hits: number; warnings: number; error?: string };
export type Progress = { scan_id: string; phase: string; enumerated: number; processed: number; cache_hits: number; warnings: number; elapsed_ms: number; items_per_second: number; eta_seconds?: number };
export type Workflow = { id: string; success: number; skipped: number; failed: number };
export type History = { id: string; kind: string; mode?: string; status: string; started_at: number; finished_at?: number; parent_id?: string; root_scan_id: string; success: number; skipped: number; failed: number };
export type RunDetail = { id: string; kind: string; status: string; parent_id?: string; success: number; skipped: number; failed: number };
export type PlanItem = { id: string; conflict_group_id?: string; conflict_member_count: number; ordinal: number; source_path: string; target_path?: string; action: string; risk: string; reason?: string };
export type PlanItemCounts = { moves: number; skips: number; needs_attention: number; conflicts: number; invalid_target: number; metadata_missing: number; path_too_long: number };
export type PlanItemPage = { items: PlanItem[]; total: number; filtered_total: number; next_cursor: number | null; counts: PlanItemCounts };
export type ConflictMember = { item_id: string; ordinal: number; source_path: string };
export type PlanConflictDetail = { id: string; kind: string; target_path: string; existing_target_path?: string; members: ConflictMember[]; candidates: { target_path: string; members: ConflictMember[] }[] };
export type Log = { id: string; execution_id: string; sequence_no: number; source_path: string; target_path?: string; action: string; result: string; error?: string; created_at: number };
export type Metric = { phase: string; elapsed_ms: number; item_count: number };
export type DuplicateStrategy = "skip" | "sequence" | "template";
export type NamingRules = { artist_dir_template: string; album_dir_template: string; disc_dir_template: string; filename_template: string; duplicate_suffix_template: string; use_source_filename: boolean; use_source_image_filename: boolean; allow_missing_metadata: boolean; duplicate_strategy: DuplicateStrategy };
export type NamingField = "artist_dir_template" | "album_dir_template" | "disc_dir_template" | "filename_template" | "duplicate_suffix_template";
export type NamingPreview = { relative_path: string; issues: { field: string; code: string; message: string }[] };
export type CleanupPreview = { plans: number; executions: number; logs: number; blocked: boolean };

export const defaultNaming: NamingRules = { artist_dir_template: "{album_artist}", album_dir_template: "{album}", disc_dir_template: "[{disc_no:02d}]", filename_template: "[{track_no:02d}_]{title}{extension}", duplicate_suffix_template: "_{disc_no:02d}", use_source_filename: false, use_source_image_filename: false, allow_missing_metadata: false, duplicate_strategy: "skip" };
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

export function sourceFileName(path: string) { return path.split(/[\\/]/).at(-1) ?? "image"; }
export function joinPath(directory: string, filename: string) { return `${directory.replace(/[\\/]$/, "")}\\${filename}`; }
export function loadNaming(): NamingRules { try { return { ...defaultNaming, ...JSON.parse(localStorage.getItem("mfb.naming") ?? "{}") as Partial<NamingRules> }; } catch { return defaultNaming; } }
