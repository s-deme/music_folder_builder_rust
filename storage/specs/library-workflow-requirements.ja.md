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

### REQ-UI-002: 操作導線

WHEN 利用者が実行操作を選択する場合、THEN UI SHALL Scan、Plan、Dry-run、Apply、Verify、Rollback を段階表示し、dry-run と本実行を色・文言・確認操作で区別する。
