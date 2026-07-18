# UI 設計

日本語を既定に `i18n.t(key)` で文言を管理する。React Query 相当の cursor API cache と virtualized table を使う。

- ダッシュボード: run status、処理済み、件/秒、警告、ETA、phase duration。
- Workflow: Scan → Plan → Dry-run → Apply → Verify → Rollback。Apply と rollback は plan/execution ID、危険/衝突件数、確認文言を示し、本実行は dry-run と別ボタン・確認 dialog にする。
- Plan list: cursor pagination、検索、sort、risk/conflict filter、行詳細、target の手動指定。指定は「改訂 plan を作成」と明示し、元 plan を更新しない。大量データを event/response に丸ごと載せない。page response は `items`、plan全体の `total`、現在条件の `filtered_total`、`next_cursor`、検索語に該当する action/risk 別 `counts` を返す。`next_cursor=null` を末尾として追加読込を終了し、追加要求中はボタンを無効化する。応答は request 世代を照合して古い filter の応答を破棄し、IDによる重複排除と `filtered_total` 上限を適用する。一覧上部には全件・移動・スキップ・要確認とrisk内訳、一覧下部には「読込済み / 絞り込み該当」を表示する。
- Plan item: ordinal、action、riskをheaderに置き、source、target、reasonを日本語label付きの縦配置にする。pathは省略表示して一覧の横scrollを発生させず、title属性で全文を確認可能にする。衝突、メタデータ不足、移動先不正、画像対応、長さ超過を含むすべてのrisk/action/reason内部codeは意味の分かる日本語へ変換する。未知codeも日本語fallbackを表示し、内部codeを理由本文へそのまま露出しない。長さ超過のreasonは対象、実測文字数、上限文字数（例: `パス全体が長すぎます: 241文字（上限240文字）`）を表示する。同一Plan内の衝突itemには「衝突相手を表示」を置き、展開すると共通の移動先と自分を含む全sourceをitem番号付きで並べる。各pathは全文copy可能とし、相手が未読込pageにあってもdetail APIから取得する。既存targetとの衝突はdry-run/apply結果でsourceと既存ファイルpathを併記する。target変更は情報より弱いsecondary actionとして配置する。
- Naming: 標準、discなし、年付き、元ファイル名保持、customのプリセットを起点に、artist/album/disc/filenameを日本語ラベル付きフォームで編集する。field tokenは選択挿入でき、元ファイル名保持時は競合するtemplate入力を無効化する。内部JSONは通常UIへ露出しない。
- Metadata不足: 命名設定で「元のフォルダ構成で移動（推奨）」「メタデータ不足フォルダへ移動」「スキップ」を選択する。専用フォルダ名は該当方針の選択時だけ表示・検証する。Plan itemは移動可能でも要確認として `metadata_missing` riskを表示し、metadata読取不能、artist不足、album不足を日本語で区別する。
- Naming preview: Coreのvalidation/previewを入力変更時に呼び、サンプルmetadataによる階層表示と構文・未知field・空component・Windows path riskをPlan前に日本語で示し、内部codeを理由本文へ露出しない。長さ超過時は対象、実測文字数、上限文字数を表示する。error時はPlan作成を無効化する。
- Duplicate: skip、安定連番、custom suffixを明示的に選択し、custom選択時だけsuffix templateを表示する。
- Logs/history: Tauri event を append-only ring buffer に表示し、完全履歴は SQLite cursor query で取得する。履歴は日時、種別の日本語ラベル、状態 badge、結果集計を列にした選択可能な table とし、内部 ID は短縮表示する。種別・状態・ID filter、新旧 sort、`(started_at,id)` の複合 cursor を server side で処理する。同じ scan 由来の run は workflow group として区切る。選択した run は別の detail panel に完全 ID と copy、親 run、開始・終了・所要時間、集計、「この実行を開く」を表示する。削除は detail panel の副次的かつ危険な操作とし、completed run は依存 run も含む削除予定件数を確認してから整理でき、running run は削除 UI を無効化する。空、loading、末尾到達、error の各状態を文言で示す。
- 状態 DB は Desktop backend がユーザー別ローカルアプリデータ領域に自動配置する。workflow UI に DB path の入力欄を置かず、DB path 解決完了後に履歴読込と操作を有効化する。
- theme: light/dark/system。色だけに依存せず dry-run/breaking action をラベルでも区別する。
