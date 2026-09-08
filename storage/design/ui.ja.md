# UI 設計

日本語を既定に `i18n.t(key)` で文言を管理する。cursor query cacheとvirtualized tableを使う。UIはfilesystem/DBの権限主体ではなく、backend managed application serviceへtyped commandを送るviewとする。

## Workflow reducer

workflow stateは散在する `useState` や「IDがtruthyか」で制御せず、一つのdiscriminated unionとpure reducerで管理する。

```text
booting
  -> idle
  -> scanning(job) -> scan_ready(scan_id)
  -> planning(job) -> plan_ready(plan_id, snapshot_hash)
  -> preflighting(job) -> dry_run_ready(execution_id)
  -> applying(job) -> applied(execution_id) | recovery_required(execution_id)
  -> verifying(job, VerifySubject) -> verified(verify_id)
  -> rolling_back(job) -> rolled_back(rollback_id) -> verifying(...Rollback)
  -> recovering(job) -> recovered(recovery_id) -> verifying(...Recovery)
```

stateは `workflow_generation`、root scan ID、active job、current Plan、selected immutable attempt、recovery情報を持つ。新scan開始でPlan以降、新Plan/revision完成でexecution以降をreducer actionとして必ず破棄する。旧execution IDを新PlanのVerify/Rollbackに流用しない。button enabled/disabledはstate tagとtyped selectorだけから導出する。

request/response/eventには `job_id`、`workflow_generation`、run/attempt ID、単調増加 `event_seq` を含める。reducerはgeneration/job不一致、完了済みjobの遅延event、既処理sequenceを破棄する。filter/cursor queryもquery generationを比較し、古いresponseをmergeしない。error/cancelは元の安全なready stateと新attempt IDを明示して遷移し、boolean `busy` 一つで全状態を表さない。

## Backend job model とsecurity boundary

Desktop backendの `JobRegistry` がjob ID、cancel token、run ID、phase、latest sequence、terminal resultを所有する。長時間commandはjobを作って直ちにhandleを返し、eventは進捗通知に限定する。WebView再読込・event欠落時は `get_job_snapshot(job_id, after_seq)` とDBのattempt queryから権威状態を復元する。filesystem mutation jobはDB/OS leaseにより一つだけ、cancelはsafe pointで受理し、`published` 以後はjournalを曖昧にせず完了または `recovery_required` にする。

Desktop backendは起動時に `app_local_data_dir/music-folder.db` を解決した `Arc<ApplicationService>` をmanaged stateへ置く。Tauri commandは `database`/任意path引数を取らない。source/target folder選択は明示したdialog capabilityで取得し、backendがLosslessWindowsPath/SafeTargetPathとして再検証する。CSPはself + 必須IPC/assetだけ、remote navigation/script/style、`unsafe-eval`、shell、広いfilesystem capabilityを禁止する。CSP/capability release設定をintegration testで検査する。

手動target改訂は二段階commandにする。`authorize_plan_target` だけが表示用の候補文字列を受け取り、backendはpersist済みPlanのlossless target root、rules version、plan item、workflow generationに対してCoreの `SafeTargetPath` validatorを実行する。成功時はtarget自体をWebViewへ権限として返さず、Plan ID・item ID・generation・検証済みpathに束縛した推測不能・10分以内・一回限りのcapability IDを発行する。`revise_plan_target` はraw targetを引数に持たず、そのcapability IDを原子的にconsumeした後もCore改訂use caseで再検証する。binding不一致、期限切れ、replay、stale generationはfilesystem/DB変更前に拒否する。

- ダッシュボード: run status、処理済み、件/秒、警告、ETA、phase duration。
- Workflow: Scan → Plan → Dry-run → Apply → Verify → Rollback/Recovery。Apply とrollbackはplan/execution ID、Advisory/Blocking件数、衝突件数、確認文言を示し、本実行はdry-runと別button・確認dialogにする。`risk`表示だけを理由に実行不能とせず、Blockingを解消する導線とAdvisory確認を分ける。recovery_required時は通常Apply/Rollbackを無効化し、Recovery dry-run、temp/source/targetの観測結果、resume/rollback/discard候補を専用panelに表示する。
- Plan list: cursor pagination、検索、sort、risk/conflict filter、行詳細、target の手動指定。指定は「改訂 plan を作成」と明示し、元 plan を更新しない。大量データを event/response に丸ごと載せない。page response は `items`、plan全体の `total`、現在条件の `filtered_total`、`next_cursor`、検索語に該当する action/risk 別 `counts` を返す。`next_cursor=null` を末尾として追加読込を終了し、追加要求中はボタンを無効化する。応答は request 世代を照合して古い filter の応答を破棄し、IDによる重複排除と `filtered_total` 上限を適用する。一覧上部には全件・移動・スキップ・要確認とrisk内訳、一覧下部には「読込済み / 絞り込み該当」を表示する。
- Plan item: ordinal、action、riskをheaderに置き、source、target、reasonを日本語label付きの縦配置にする。pathは省略表示して一覧の横scrollを発生させず、title属性で全文を確認可能にする。衝突、メタデータ不足、移動先不正、画像対応、長さ超過を含むすべてのrisk/action/reason内部codeは意味の分かる日本語へ変換する。未知codeも日本語fallbackを表示し、内部codeを理由本文へそのまま露出しない。長さ超過のreasonは対象、実測文字数、上限文字数（例: `パス全体が長すぎます: 241文字（上限240文字）`）を表示する。同一Plan内の衝突itemには「衝突相手を表示」を置き、展開すると共通の移動先と自分を含む全sourceをitem番号付きで並べる。各pathは全文copy可能とし、相手が未読込pageにあってもdetail APIから取得する。既存targetとの衝突はdry-run/apply結果でsourceと既存ファイルpathを併記する。target変更は情報より弱いsecondary actionとして配置する。
- Image destination conflict: 移動先欄を `未決定（候補N件）` とし、「移動先候補を確認」から全candidate directory、対応音楽件数、音楽item番号/source pathを表示する。各candidateに「この移動先を選択」を置き、選択時は改訂Planを作成する。
- Naming: 標準、discなし、年付き、元ファイル名保持、customのプリセットを起点に、artist/album/disc/filenameを日本語ラベル付きフォームで編集する。field tokenは選択挿入でき、元ファイル名保持時は競合するtemplate入力を無効化する。内部JSONは通常UIへ露出しない。
- Long path option: 命名設定に既定OFFの「Windowsの長いパスを許可」を置く。ON時は「Windows側の長いパス設定が必要で、環境によって失敗する」旨を近接表示し、240文字超のpreviewをerrorにしない。Plan itemには長いpathを許可していることと実測文字数を要確認情報として表示する。
- Metadata不足: 「メタデータ不足を無視してフォルダを作成する」checkboxを命名設定に置き、既定は無効とする。無効時は不足itemをskipする。有効時はartist不足を `Unknown Artist`、album不足を `Unknown Album` で補完し、読み取れたmetadataと通常の命名規則を使って移動する。metadata全体を読めない場合は両方を補完して元ファイル名を保持する。Plan itemは移動可能でも要確認として `metadata_missing` riskを表示し、metadata読取不能、artist不足、album不足を日本語で区別する。
- Naming preview: Coreのvalidation/previewを入力変更時に呼び、サンプルmetadataによる階層表示と構文・未知field・空component・Windows path riskをPlan前に日本語で示し、内部codeを理由本文へ露出しない。長さ超過時は対象、実測文字数、上限文字数を表示する。error時はPlan作成を無効化する。
- Duplicate: skip、安定連番、custom suffixを明示的に選択し、custom選択時だけsuffix templateを表示する。
- Logs/history: Tauri eventはbounded ring bufferへ表示し、完全履歴はSQLite cursor queryで取得する。履歴は日時、attempt種別、VerifySubject、日本語状態badge、結果集計を列にした選択可能なtableとし、内部IDは短縮表示する。種別・状態・ID filter、新旧sort、`(started_at,id)`の複合cursorをserver sideで処理する。同じscan由来のrun/attemptはworkflow groupとして区切る。detail panelは完全ID/copy、親/subject、開始・終了・所要時間、attempt固有log/journal、preflight結果、「この実行を開く」を表示する。複数Verify/Rollback attemptは混ぜずtime lineに並べる。空、loading、末尾到達、error、retentionで省略済みの各状態を文言で示す。
- Archive/history cleanup: 既定の「履歴を整理」はhard deleteではなくworkflow archive previewを開き、対象run/attempt/log件数、archive先、推定容量を確認する。running、lease中、recovery_requiredは無効化する。archive完了後はdigest検証済みbadgeを表示し、archive済みfilterから参照できる。active DBからのpurgeは別の危険操作とし、verified archive、削除対象、復元方法、媒体fileを削除しないことを確認する。
- 状態DBはDesktop backendがユーザー別local app dataへ自動配置する。workflow UIにDB pathの入力欄を置かず、backend initialization完了後にjob/history操作を有効化する。表示用pathを返す場合も編集・commandへ再送しない。
- theme: light/dark/system。色だけに依存せず dry-run/breaking action をラベルでも区別する。

## Lossless path 表示

backend DTOはpathを `{encoding, raw_base64, display}` で返す。UIはdisplayを主表示するが、copy/exportは可能ならlossless envelopeまたはnative backend copy commandを使い、置換文字を含む場合は「表示できない文字を含む」badgeを出す。display文字列をtarget revision requestの権威値として送り返さず、item ID/candidate IDまたはfolder dialog tokenを送る。manual target previewはbackendのSafeTargetPath validation結果が成功するまでPlan revisionを作れない。

## Test strategy

- reducer transition table test: 全state/action、new scan/Plan revision時の下流ID消去、cancel/error/recovery、Rollback後VerifySubjectを検証する。
- race/property test: event順序入替、重複、欠落、古いgeneration、job snapshotとの再同期で不正buttonが有効にならないことを検証する。
- component/accessibility test: Blocking/Advisory、確認dialog、keyboard/focus、色に依存しないbadge、loading/empty/error/末尾を検証する。
- command contract test: commandにDB pathが存在しないこと、typed error/attempt ID/lossless path envelope、CSP/capability allow-listを固定する。
- integration/E2E: temp app-data DBを使い、ScanからRollback後Verify、crash fixtureからRecovery、archive、WebView reload/job再同期を実行する。filesystem mutationはfixture root外へ出ないことをbackendでもassertする。
- UI typecheckとproduction buildを正規Docker validationへ含め、Windows固有Tauri/path/job testはWindows CIで実行する。
