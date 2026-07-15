# UI 設計

日本語を既定に `i18n.t(key)` で文言を管理する。React Query 相当の cursor API cache と virtualized table を使う。

- ダッシュボード: run status、処理済み、件/秒、警告、ETA、phase duration。
- Workflow: Scan → Plan → Dry-run → Apply → Verify → Rollback。Apply と rollback は plan/execution ID、危険/衝突件数、確認文言を示し、本実行は dry-run と別ボタン・確認 dialog にする。
- Plan list: cursor pagination、検索、sort、risk/conflict filter、行詳細、target の手動指定。指定は「改訂 plan を作成」と明示し、元 plan を更新しない。大量データを event/response に丸ごと載せない。
- Naming: artist/album/disc/filename/duplicate suffix templates、元音楽・画像ファイル名保持を編集し、Plan 作成時に rules snapshot として保存する。
- Logs/history: Tauri event を append-only ring buffer に表示し、完全履歴は SQLite cursor query で取得する。completed run は依存 run も含む削除予定件数を確認してから整理でき、running run は削除 UI を無効化する。
- theme: light/dark/system。色だけに依存せず dry-run/breaking action をラベルでも区別する。
