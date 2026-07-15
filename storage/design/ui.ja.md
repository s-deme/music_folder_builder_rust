# UI 設計

日本語を既定に `i18n.t(key)` で文言を管理する。React Query 相当の cursor API cache と virtualized table を使う。

- ダッシュボード: run status、処理済み、件/秒、警告、ETA、phase duration。
- Workflow: Scan → Plan → Dry-run → Apply → Verify → Rollback。Apply と rollback は plan/execution ID、危険/衝突件数、確認文言を示し、本実行は dry-run と別ボタン・確認 dialog にする。
- Plan list: cursor pagination、検索、sort、risk/conflict filter、行詳細、target の手動指定。指定は「改訂 plan を作成」と明示し、元 plan を更新しない。大量データを event/response に丸ごと載せない。page response は `items`、plan全体の `total`、現在条件の `filtered_total`、`next_cursor`、検索語に該当する action/risk 別 `counts` を返す。一覧上部には全件・移動・スキップ・要確認とrisk内訳、一覧下部には「読込済み / 絞り込み該当」を表示する。
- Plan item: ordinal、action、riskをheaderに置き、source、target、reasonを日本語label付きの縦配置にする。pathは省略表示して一覧の横scrollを発生させず、title属性で全文を確認可能にする。risk/actionの内部codeは日本語labelへ変換し、target変更は情報より弱いsecondary actionとして配置する。
- Naming: 標準、discなし、年付き、元ファイル名保持、customのプリセットを起点に、artist/album/disc/filenameを日本語ラベル付きフォームで編集する。field tokenは選択挿入でき、元ファイル名保持時は競合するtemplate入力を無効化する。内部JSONは通常UIへ露出しない。
- Naming preview: Coreのvalidation/previewを入力変更時に呼び、サンプルmetadataによる階層表示と構文・未知field・空component・Windows path riskをPlan前に示す。error時はPlan作成を無効化する。
- Duplicate: skip、安定連番、custom suffixを明示的に選択し、custom選択時だけsuffix templateを表示する。
- Logs/history: Tauri event を append-only ring buffer に表示し、完全履歴は SQLite cursor query で取得する。履歴は日時、種別の日本語ラベル、状態 badge、結果集計を列にした選択可能な table とし、内部 ID は短縮表示する。種別・状態・ID filter、新旧 sort、`(started_at,id)` の複合 cursor を server side で処理する。同じ scan 由来の run は workflow group として区切る。選択した run は別の detail panel に完全 ID と copy、親 run、開始・終了・所要時間、集計、「この実行を開く」を表示する。削除は detail panel の副次的かつ危険な操作とし、completed run は依存 run も含む削除予定件数を確認してから整理でき、running run は削除 UI を無効化する。空、loading、末尾到達、error の各状態を文言で示す。
- theme: light/dark/system。色だけに依存せず dry-run/breaking action をラベルでも区別する。
