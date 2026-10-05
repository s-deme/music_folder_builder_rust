import type { FolderSelection, LosslessPathEnvelope, Progress } from "./model";

export type Page<T> = { items: T[]; total: number; next_cursor: number | null };
export type DoctorRun = { doctor_run_id: string; scan_run_id: string; status: string; files: number; cache_hits: number; failures: number; issue_count: number; rule_version: number; error?: string };
export type DoctorHistory = { run: DoctorRun; source: LosslessPathEnvelope; started_at: number; finished_at: number | null };
export type DoctorSummary = { history: DoctorHistory; severities: Record<string, number> };
export type DoctorSnapshot = { request_id: string; status: string; doctor_run_id: string | null; progress: Progress | null; error: string | null };
export type IssueRow = { ordinal: number; code: string; severity: string; category: string; file_count: number };
export type FileRow = { id: string; path: LosslessPathEnvelope; metadata: { title?: string; artist?: string; album_artist?: string; album?: string; track_no?: number; disc_no?: number; year?: number; genre?: string; has_artwork?: boolean }; format: string; size_bytes: number; mtime_ns: string; sha256?: string; identity?: string };
export type IssueDetail = { issue: IssueRow; evidence: string[]; comparison: string | null; rule_version: number; files: Page<FileRow> };
export type AlbumRow = { id: string; title: string; artist: string; folder: LosslessPathEnvelope; tracks: number; issue_count: number; unclassified: boolean };
export type AlbumDetail = { album: AlbumRow; files: Page<FileRow>; issues: Page<IssueRow> };
export type Artwork = { data_url: string | null; origin: string | null; source: LosslessPathEnvelope | null; note: string };
export type DoctorPanelProps = { ready: boolean; visible: boolean; organizeBusy: boolean; onBusy: (busy: boolean) => void; onOrganize: (selection: FolderSelection) => Promise<void> };

export const issueLabels: Record<string, string> = {
  read_failed: "タグ読取失敗", file_changed: "診断中のファイル変更", scan_warning: "スキャン警告",
  missing_title: "Title欠損", missing_artist: "Artist欠損", missing_album: "Album欠損", missing_track: "Track Number欠損",
  missing_album_artist: "Album Artist欠損", missing_disc: "Disc Number欠損", missing_year: "Year欠損", missing_artwork: "埋め込みジャケット欠損",
  artist_variant: "Artistの表記揺れ", album_variant: "Albumの表記揺れ", exact_duplicate: "完全重複", same_file_paths: "同じ実体への別パス",
  track_gap: "トラック番号の欠番候補", track_duplicate: "トラック番号の重複", disc_inconsistent: "ディスク番号の不整合",
  album_artist_inconsistent: "Album Artistの不一致", year_inconsistent: "Yearの不一致", genre_inconsistent: "Genreの不一致", format_mixed: "ファイル形式の混在",
};
export const severityLabels: Record<string, string> = { critical: "Critical · 根拠の取得不可", warning: "Warning · 要確認", info: "Info · 参考情報" };
export function doctorStatus(status: string) {
  return ({ running: "未完了", completed: "完了", partial: "部分失敗", cancelled: "取消済み", failed: "失敗" } as Record<string, string>)[status.toLowerCase()] ?? status;
}
