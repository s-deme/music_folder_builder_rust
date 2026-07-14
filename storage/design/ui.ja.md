# UI 設計

日本語を既定に `i18n.t(key)` で文言を管理する。React Query 相当の cursor API cache と virtualized table を使う。

- ダッシュボード: run status、処理済み、件/秒、警告、ETA、phase duration。
- Workflow: Scan → Plan → Dry-run → Apply → Verify → Rollback。Apply は plan ID、危険/衝突件数、確認文言を示す。
- Plan list: cursor pagination、検索、sort、risk/conflict filter、行詳細。大量データを event/response に丸ごと載せない。
- Logs/history: Tauri event を append-only ring buffer に表示し、完全履歴は SQLite cursor query で取得する。
- theme: light/dark/system。色だけに依存せず dry-run/breaking action をラベルでも区別する。
