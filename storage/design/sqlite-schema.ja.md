# SQLite スキーマ設計

起動時に `PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;` を設定する。migration table を用い、writer は 250 件または短時間間隔で transaction commit する（値は設定化）。読み取りは専用接続で行う。

| Table | 主な列 | 用途 |
|---|---|---|
| schema_migrations | version, applied_at | schema 版管理 |
| scan_runs | id, source_root, status, started_at, finished_at, config_json | scan 単位 |
| library_files | id, canonical_path, size_bytes, mtime_ns, file_identity, link_state, last_seen_scan_id | 現在/既知ファイル |
| metadata_cache | file_id, fingerprint, reader_version, tag_json, status, error | 差分タグ再利用 |
| scan_items | scan_run_id, file_id, disposition, warning | 音楽・asset を含む scan snapshot |
| plan_runs | id, scan_run_id, parent_plan_id, rules_json, rules_version, snapshot_hash, status | immutable plan 親 |
| plan_items | id, plan_run_id, file_id, ordinal, action, source_path, target_path, conflict, risk, reason, target_origin | apply 入力 |
| execution_runs | id, plan_run_id, mode, status, counters | dry-run/apply |
| operation_logs | id, execution_run_id, plan_item_id, sequence_no, action, result, source_deleted, error | apply 監査 |
| verify_runs / verify_logs | id, subject_run_id, status / item result | 検証監査 |
| rollback_runs / rollback_logs | id, execution_run_id, status / operation_log_id, sequence_no, result | 巻戻し監査 |
| run_metrics | run_id, phase, elapsed_ms, item_count, bytes | phase 計測 |
| event_logs | run_id, level, event, payload_json, created_at | UI/診断ログ |

主要 index は `library_files(canonical_path)` unique、`scan_items(scan_run_id,file_id)`、`plan_items(plan_run_id,ordinal)`、`operation_logs(execution_run_id,sequence_no)`、各 run 外部キーと status。path は Windows canonical/display の両方を必要に応じ保存し、日時は UTC、サイズは INTEGER、JSON は監査用 payload としてのみ使う。

plan revision は `parent_plan_id` を持つ新しい `plan_runs` と、再評価済みの全 `plan_items` を同一 transaction で保存する。`target_origin` は `rule` または `manual`、手動指定の理由は event_logs payload に保存する。履歴削除は run の依存グラフを子から親へ削除する transaction とし、running status を事前に拒否する。plan itemは targetを再計算せず、後続applyの唯一の入力になる。
