# 要件定義: library-workflow（EARS）

## 範囲

初回リリースは scan、plan、dry-run apply、apply、verify、rollback、履歴照会、および Tauri desktop UI/CLI を対象とする。Python 版の Tkinter 実装、既存 DB の直接アップグレード、クラウド同期は対象外である。

### REQ-SAF-001: 段階型ワークフロー

WHEN 利用者が整理を実行する場合、THEN システム SHALL `scan -> plan -> apply -> verify -> rollback` の状態遷移を保持する。

### REQ-SAF-002: Apply の入力固定

WHEN apply を開始する場合、THEN システム SHALL 永続化済みかつ完了した `plan_run_id` の items のみを実行し、scan 結果・UI フィルタ・タグから移動先を再計算しない。

### REQ-SAF-003: Dry-run

WHEN dry-run を要求された場合、THEN システム SHALL move/copy/delete/overwrite を行わず、item ごとの予測と execution run を保存し、本実行と明確に区別する。

### REQ-SAF-004: 既存 target と危険状態

IF target が存在する、path risk/conflict がある、または検証に失敗する場合、THEN システム SHALL 当該 item を skip/failed として記録し、唯一の既知コピーを削除しない。

### REQ-SCN-001: 高速かつ有界な scan

WHEN scan が source root を走査する場合、THEN システム SHALL 列挙、上限付き並列タグ読取、単一 SQLite writer、進捗通知の pipeline で処理し、キュー容量を設定可能にする。

### REQ-SCN-002: Reparse point

WHEN reparse point を検出した場合、THEN システム SHALL 既定で追跡せず、除外理由を保存する。明示 opt-in 時だけ追跡を許可する。

### REQ-SCN-003: 差分再利用

IF canonical path、size、mtime（必要に応じ file identity）が既読値と一致する場合、THEN システム SHALL タグ読取を再利用可能にし、再利用/再読取の根拠を保存する。

### REQ-PLN-001: Windows path policy

WHEN plan が target path を生成する場合、THEN システム SHALL Unicode を保持し、禁止文字、末尾空白/ピリオド、予約名、component/path 長を判定し、変換と risk 理由を保存する。

### REQ-PLN-002: 命名規則

WHEN 利用者が plan を作成する場合、THEN システム SHALL artist、album、disc、filename のテンプレートを用いて target を生成し、`artist`、`album_artist`、`album`、`title`、`track_no`、`disc_no`、`year`、`extension`、`source_stem` を参照可能にする。テンプレートは数値書式と、値がない場合に全体を省略する条件ブロックを提供する。

WHEN 利用者が命名規則を編集する場合、THEN システム SHALL 用途別プリセット、日本語ラベル付きフィールド、利用可能なフィールドの挿入、既定値への復元を提供し、内部JSONの直接編集を要求してはならない。

WHEN 命名規則が変更された場合、THEN システム SHALL 構文、未知フィールド、空の必須componentおよびWindows path riskを検証し、サンプルmetadataから生成される相対pathをPlan作成前に表示する。

### REQ-PLN-003: 重複 target の解決

IF 複数 item が同一 target となる場合、THEN システム SHALL 設定済みの duplicate suffix template により決定的かつ一意な target を生成する。suffix で解決できない衝突は skip として保存し、既存 target を上書きしてはならない。

WHEN 利用者が重複処理を設定する場合、THEN システム SHALL `skip`、安定した`sequence`、templateによるsuffixを明示的に選択可能にし、選択結果をrules snapshotへ保存する。

### REQ-PLN-004: 手動 target 指定

WHEN 利用者が plan item の target を指定する場合、THEN システム SHALL 元の completed plan を変更せず、指定内容・根拠・親 plan ID を記録した新しい immutable plan を作成する。新 target は通常の path policy、重複検査、snapshot hash 検証の対象とする。

### REQ-MDA-001: album artist

WHEN 音声 metadata を読む場合、THEN システム SHALL 対応形式の album artist を取得し、取得不能時だけ artist へのフォールバックを許可する。

### REQ-AST-001: 同梱画像

WHEN scan が音楽ファイルと同じ source tree 内の対応画像を検出した場合、THEN システム SHALL 画像を scan snapshot に保存し、plan で対応する音楽の target directory へ移動予定を作成する。対応先がない、または一意に決定できない画像は skip と理由を記録する。

### REQ-AST-002: 画像名

WHEN 同梱画像の target を作成する場合、THEN システム SHALL source image filename を保持する設定を提供する。保持時に target が重複する場合は `_2` から始まる決定的な連番を付け、既存 target を上書きしてはならない。

### REQ-APL-001: 異ボリューム適用

WHEN source と target が異ボリュームの場合、THEN システム SHALL `copy -> verify -> source delete` の順で直列実行し、verify 失敗時に source を削除しない。同一ボリュームは rename/move を用いる。

### REQ-VRF-001: 検証

WHEN apply または rollback 後に verify を実行する場合、THEN システム SHALL 対応する実行ログを根拠に存在・サイズ・必要時の fingerprint を検査し、item ごとの結果を保存する。

### REQ-RBK-001: 巻き戻し

WHEN rollback を開始する場合、THEN システム SHALL 成功した execution log のみを逆 sequence で処理する。異ボリュームでは `copy -> verify -> target delete` を用いる。

### REQ-OBS-001: 永続化と計測

WHEN run が進行・完了・中断する場合、THEN システム SHALL 状態、件数、警告、ログ、検証結果、および enumerate/tag_read/db_write/plan/apply/verify の duration を SQLite に保存する。

### REQ-UI-001: 大量データ UI

WHEN UI が plan または履歴一覧を表示する場合、THEN システム SHALL cursor/page API と仮想スクロールを用い、全 item を WebView へ一括送信しない。

WHEN UI が plan 一覧を表示する場合、THEN システム SHALL plan 全件数、現在の検索・filterに該当する件数、WebViewへ読込済みの件数、および risk/action 別件数を区別して表示する。

WHEN UI が長い source/target path を表示する場合、THEN システム SHALL 一覧全体を横方向へ拡張せず、source、target、理由を識別可能なラベルと省略・展開可能な表示を提供する。

### REQ-UI-002: 操作導線

WHEN 利用者が実行操作を選択する場合、THEN UI SHALL Scan、Plan、Dry-run、Apply、Verify、Rollback を段階表示し、dry-run と本実行を色・文言・確認操作で区別する。

### REQ-UI-003: 履歴整理

WHEN 利用者が履歴を削除する場合、THEN システム SHALL 対象 run と従属する plan、execution、verify、rollback、log を transaction 内で削除し、実ファイルを変更してはならない。実行中 run は削除してはならない。

### REQ-UI-004: Desktop rollback

WHEN 利用者が Desktop から rollback を実行する場合、THEN UI SHALL dry-run と本実行を別の操作として示し、本実行には execution ID、対象件数、不可逆な削除を含む確認操作を要求する。
