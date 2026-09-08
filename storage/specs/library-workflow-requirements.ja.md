# 要件定義: library-workflow（EARS）

## 範囲

初回リリースは scan、plan、dry-run apply、apply、verify、rollback、履歴照会、および Tauri desktop UI/CLI を対象とする。Python 版の Tkinter 実装、既存 DB の直接アップグレード、クラウド同期は対象外である。

## 規範用語

- `scan snapshot` は、completed scan が列挙時点で観測した path、file identity、fingerprint、metadata および診断の不変な集合である。後続 scan が更新する metadata cache とは別物とする。
- `execution eligibility` は item が filesystem mutation の候補になれるかを表す `eligible` / `blocked` の判定である。`warning` は利用者へ知らせる非阻害の診断であり、eligibility と別に保持する。既存データの `risk` は versioned migration でいずれかへ明示的に写像する。
- `operation attempt` は apply、rollback、verify または recovery の1回の試行であり、同じ論理 run の再試行も新しい attempt ID を持つ。
- `VerifySubject` は verify の根拠を特定する型であり、少なくとも `ApplyAttempt` と `RollbackAttempt` を区別する。
- Windows path は、filesystem API に渡す可逆な native path、利用者向け display path、および衝突判定専用 comparison key を区別する。display path や comparison key から native path を復元してはならない。

### REQ-SAF-001: 段階型ワークフロー

WHEN 利用者が整理を実行する場合、THEN システム SHALL `scan -> plan -> apply -> verify -> rollback` の状態遷移を保持する。

WHEN dry-run、recovery または rollback 後 verify を含む分岐を表現する場合、THEN システム SHALL 親 run、operation attempt、および `VerifySubject` を型付けした workflow state として保持し、単に「最後に実行した ID」から次の操作対象を推測してはならない。

### REQ-SAF-002: Apply の入力固定

WHEN apply を開始する場合、THEN システム SHALL 永続化済みかつ完了した `plan_run_id` の items のみを実行し、scan 結果・UI フィルタ・タグから移動先を再計算しない。

WHEN completed plan の snapshot hash を生成または検証する場合、THEN システム SHALL rules/schema version、scan snapshot ID/hash、absolute target root、および filesystem mutation を認可する plan item の `ordinal`、losslessな`source_path`、source identity、nullableな`target_path`、`action`、`execution_eligibility`、順序を正規化したwarning code、`reason` を同一のversioned正規形式で計算し、画像の移動先候補や衝突相手など apply 入力ではない診断情報を含めてはならない。

IF 永続化済みの apply 入力項目が snapshot 作成後に変更された場合、THEN システム SHALL dry-run を含む apply を mutation 前に拒否する。

WHEN plan を completed にする場合、THEN システム SHALL plan header、全 apply 入力item、rules snapshot、および snapshot hash を同一transactionで確定し、以後は更新せず、修正を immutable な子 plan として作成する。

### REQ-SAF-003: Dry-run

WHEN dry-run を要求された場合、THEN システム SHALL persisted planの検証、source identity、target existence、target confinement、reparse、eligibility、content verification可否、および衝突の判定について本applyと同一のvalidation pipelineをmutation直前まで実行し、itemごとの予測と観測時刻を持つexecution runを保存する。

WHEN dry-run を実行する場合、THEN システム SHALL move、copy、delete、rename、directory作成、temporary file作成、journal上のmutation phase進行またはtarget予約を一切行わず、結果を `predicted` として本applyの実績から明確に区別する。

WHEN dry-run 後に本applyを開始する場合、THEN システム SHALL dry-run結果を認可根拠として再利用せず、変化し得る全preconditionを再検証する。

### REQ-SAF-004: 既存 target と危険状態

IF final target が存在する、targetの原子的なno-replace確定に失敗する、execution eligibilityが`blocked`である、またはidentity/content検証に失敗する場合、THEN システム SHALL 当該 item をskip/failedとして記録し、既存targetを変更せず、唯一の既知コピーを削除しない。

IF plan作成後かつpublish直前までの間に別processがtargetを作成した場合、THEN システム SHALL filesystemの原子的no-replace結果を正として衝突を記録し、事前の`exists`確認成功を上書き許可として扱ってはならない。

### REQ-SAF-005: 操作の中間状態と終端化

WHEN apply の filesystem mutation が一部だけ成功した場合、THEN システム SHALL copy済み・source未削除などの中間状態を型付けしたitem結果として順序付きで保存し、再実行・verify・rollbackが実際の状態を判定できるようにする。

IF scan、plan、apply、verify、rollback の開始後に処理、永続化、またはworkerが失敗した場合、THEN システム SHALL runを `failed` または実績に応じた `partial` の終端状態に更新し、`running` のまま残してはならない。

IF processまたはOSが永続化更新前に異常終了した場合、THEN システム SHALL 次回起動時にstaleな`running`と未完了journalを新規mutation受付前に検出し、実ファイル状態とのreconciliationが必要な`recovery_required`へ遷移させる。

### REQ-SAF-006: 実行適格性とwarningの分離

WHEN plan itemを評価する場合、THEN システム SHALL `action`、`execution_eligibility`、0件以上のtyped warning、およびblocking reasonを別fieldとして決定し、warningの有無だけからactionをskipへ変更してはならない。

IF metadata不足許可などrules snapshotで明示的に許可された状態が安全なtargetを生成できる場合、THEN システム SHALL 当該itemを`eligible`のmove候補とし、`metadata_missing`等をwarningとして保持する。

IF path confinement違反、未解決conflict、source identity不一致、reparse policy違反、snapshot不整合などmutationを安全に認可できない状態がある場合、THEN システム SHALL 当該itemを`blocked`とし、clientからwarningやactionだけを書き換えて実行可能にしてはならない。

WHEN applyがitemを選択する場合、THEN システム SHALL persisted planのactionとeligibilityを根拠に`eligible`なmutation itemだけを実行し、warning付きeligible itemを暗黙にskipしてはならない。

IF eligibilityを持たないlegacy planを読む場合、THEN システム SHALL schema/rules versionに基づく決定的なmigrationで旧riskをwarningまたはblocking reasonへ写像するか、安全側にplan再作成を要求し、未知のriskを`eligible`と仮定してはならない。

### REQ-SAF-007: Write-ahead operation journal と recovery

WHEN apply、rollbackまたはrecoveryがfilesystem mutationを行う場合、THEN システム SHALL attempt ID、item ID、単調増加sequence、source/target/staging path、期待identity/fingerprint、操作種別、および次に行うphaseをoperation journalへcommitしてから当該mutationを開始する。

WHEN operationが進行する場合、THEN システム SHALL 少なくとも `prepared -> staged -> staged_verified -> published -> published_verified -> source_deleted -> completed` のうち該当するphaseと観測結果を順序付きで保存し、同一ボリュームrenameなど不要なphaseは理由付きで明示的に省略する。

IF write-ahead intentまたはphase結果を永続化できない場合、THEN システム SHALL 次のfilesystem mutationを開始せず、sourceを保持してattemptをfailed/recovery_requiredにする。

WHEN 未完了journalをrecoveryする場合、THEN システム SHALL source、target、staging fileの現在のidentity/contentとjournalの期待値を照合し、証明できたphaseだけをidempotentに確定または補償し、pathの存在だけを根拠にdelete、overwriteまたはblind retryを行ってはならない。

IF staging fileまたは重複copyの所有attemptと内容同一性を証明できない場合、THEN システム SHALL 自動削除せず、診断と手動対処候補を保存する。

### REQ-SAF-008: Mutation lease と並行実行

WHEN apply、rollbackまたはrecoveryを開始する場合、THEN システム SHALL canonicalなsource/target root範囲に対するexclusive mutation leaseを、SQLite上のowner/heartbeat/expiry/fencing tokenとWindows process間OS lockの双方で取得してから最初のwrite-ahead intentを作成する。OS lockは包含scopeを排他しつつ、互いに包含しないsibling scopeを過剰排他してはならない。

IF CLI、Desktop、別DB instanceまたは別processのmutation対象rootが同一または包含関係で重なる場合、THEN システム SHALL 同時mutationを拒否または待機させ、dry-run/verifyなど非mutation処理と明確に区別する。

WHILE mutation attemptが進行中である間、THEN システム SHALL lease heartbeatを更新し、各破壊的phase前にfencing tokenとprocess間OS lock所有権を再確認する。

IF leaseを失った、所有権を証明できない、またはstale leaseと未完了journalを検出した場合、THEN システム SHALL 次のmutationを停止し、既存ownerの処理完了またはrecovery reconciliationなしにleaseを奪取してはならない。

### REQ-SCN-001: 高速かつ有界な scan

WHEN scan が source root を走査する場合、THEN システム SHALL 列挙、上限付き並列タグ読取、単一 SQLite writer、進捗通知の pipeline で処理し、キュー容量を設定可能にする。

### REQ-SCN-002: Reparse point

WHEN reparse point を検出した場合、THEN システム SHALL 既定で追跡せず、除外理由を保存する。明示 opt-in 時だけ追跡を許可する。

WHEN reparse point追跡を明示opt-inする場合、THEN システム SHALL 選択値、link自体のidentity、解決先、解決先root、および循環検出結果をscan rules snapshotへ保存し、source root外へ解決するlinkまたは循環するlinkを列挙してはならない。

### REQ-SCN-003: 差分再利用

IF lossless canonical path key、size、mtime、利用可能なfile identity、metadata parser version、cache schema version、およびnormalization policy versionが既読値と一致する場合、THEN システム SHALL タグ読取結果を再利用可能にし、cache keyと再利用/再読取の根拠を保存する。

IF cache keyの一部が取得不能、不一致、未知version、破損または解析失敗である場合、THEN システム SHALL cache miss/invalidとして再読取するかtyped errorを返し、古いmetadataを現在のscan観測として黙って使用してはならない。

WHEN cacheからmetadataを再利用する場合、THEN システム SHALL 値とcache provenanceを新しいscan snapshotへcopyし、後続のcache更新・削除がcompleted scanの内容を変えないようにする。

### REQ-SCN-004: Immutable scan snapshot と source identity

WHEN scan itemを列挙する場合、THEN システム SHALL losslessなabsolute native path、root-relative path、asset種別、size、十分な精度のmtime、利用可能なvolume/file identity、reparse state、metadata値/status/provenance、およびfingerprintの取得statusと取得済みの場合のalgorithm/version/valueをscan固有rowとして保存する。

WHEN scanをcompletedにする場合、THEN システム SHALL scan rules/cache version、全scan item、およびversioned snapshot hashを同一transactionで確定し、そのscanに属するrowを後続scanやcache UPSERTで変更してはならない。

IF completed scanと同じpathが後続scanで異なるidentity、内容またはmetadataとして観測された場合、THEN システム SHALL 新しいscan snapshotだけへ保存し、過去scanから作成されるPlanの入力を差し替えてはならない。

WHEN planを作成する場合、THEN システム SHALL completedかつhash検証済みで互換versionのscan snapshotだけをcursor順に読み、mutableなlibrary/cache tableとのjoin結果をscan時点の事実として扱ってはならない。

IF file identityがfilesystemから取得できない場合、THEN システム SHALL 取得不能という事実と代替identity tupleを保存し、後続の破壊的操作に必要なcontent fingerprintをapply前に取得・照合できないitemを`blocked`にする。

### REQ-PLN-001: Windows root・path confinement policy

WHEN source root、target rootまたはCLI/Desktop境界のfilesystem rootを受け付ける場合、THEN システム SHALL Windowsのfully-qualified absolute pathだけを許可し、空path、relative path、drive-relative path、root-relative path、および曖昧なdevice pathをmutation認可前に拒否する。

WHEN Windows pathをCore、SQLite、CLIまたはDesktop境界で保持する場合、THEN システム SHALL `Path`/`OsString`相当のnative表現またはUTF-16 code unitを復元できるversioned encodingを使用し、`to_string_lossy`相当の非可逆変換値をidentity、snapshot hash、衝突判定またはfilesystem操作へ使用してはならない。

WHEN pathを比較または表示する場合、THEN システム SHALL native pathとは別にWindows semanticsに従うcomparison keyとdisplay pathを生成し、case/区切り/prefixの正規化やUnicode display処理で元のnative pathを変更してはならない。

WHEN plan が target path を生成する場合、THEN システム SHALL Unicode を保持し、禁止文字、末尾空白/ピリオド、予約名、およびpath全体の長さを判定し、変換前後の値とtyped warning/blocking reasonを保存する。

WHEN 命名template、duplicate suffix、画像移動先または手動指定からtargetを作成する場合、THEN システム SHALL target rootに結合する前の値をroot/prefixを持たない安全なrelative pathとして検証し、`.`、`..`、absolute/UNC/device prefix、drive指定、alternate data stream、およびcomponent内へ注入されたseparatorによるroot逸脱を拒否する。

WHEN target候補を確定する場合、THEN システム SHALL lexical normalization後の全componentとfinal targetが保存済みabsolute target root配下にあることを共通の`SafeTargetPath`契約で証明し、命名、suffix、画像、manual revisionの全経路を同じconfinement検査へ通す。

IF target候補をtarget root配下とlosslessに証明できない場合、THEN システム SHALL 当該itemを`blocked`としてtargetをmutation入力にせず、文字列prefix一致だけでroot配下と判定してはならない。

WHEN target path の長さを判定する場合、THEN システム SHALL path全体が設定された有効上限以下（上限と同数を含む）なら各componentの文字数にかかわらず許可し、path全体の有効上限を超えた場合だけ`path_too_long` blocking reasonとする。

WHEN path長を算出する場合、THEN システム SHALL 選択したWindows API path formに対するUTF-16 code unit数、prefix、separatorおよび終端条件をversioned path policyで一貫して扱い、UIに表示する実測値とapply adapterの判定単位を一致させる。

WHEN 利用者が「Windowsの長いパスを許可」を有効にした場合、THEN システム SHALL 240文字を超えるtargetもPlanのmove候補として許可し、選択値をimmutableなnaming rules snapshotへ保存する。既定値は無効とする。

IF 長いパスを許可したitemのfilesystem操作がWindows環境条件またはfilesystem制約により失敗した場合、THEN システム SHALL sourceを削除せずitemをfailedとして記録し、Windowsの長いパス設定とアプリ対応を確認する日本語理由を表示する。

IF target path が長さ上限を超えた場合、THEN システム SHALL 実際の文字数と上限文字数を Plan の理由と命名 preview に日本語で表示し、内部の理由codeを利用者向け表示へ露出しない。

WHEN Plan の理由または命名 preview の検証理由を利用者へ表示する場合、THEN システム SHALL すべての内部理由codeを意味の分かる日本語へ変換し、未対応codeも内部文字列をそのまま表示せず日本語のfallbackと補助情報で示す。

### REQ-PLN-002: 命名規則

WHEN 利用者が plan を作成する場合、THEN システム SHALL artist、album、disc、filename のテンプレートを用いて target を生成し、`artist`、`album_artist`、`album`、`title`、`track_no`、`disc_no`、`year`、`extension`、`source_stem` を参照可能にする。テンプレートは数値書式と、値がない場合に全体を省略する条件ブロックを提供する。

WHEN 利用者が命名規則を編集する場合、THEN システム SHALL 用途別プリセット、日本語ラベル付きフィールド、利用可能なフィールドの挿入、既定値への復元を提供し、内部JSONの直接編集を要求してはならない。

WHEN 命名規則が変更された場合、THEN システム SHALL 構文、未知フィールド、空の必須componentおよびWindows path warning/blocking reasonを検証し、サンプルmetadataから生成される相対pathをPlan作成前に表示する。

### REQ-PLN-003: 重複 target の解決

IF 複数 item が同一 target となる場合、THEN システム SHALL 設定済みの duplicate suffix template により決定的かつ一意な target を生成する。suffix で解決できない衝突は skip として保存し、既存 target を上書きしてはならない。

IF item が重複 target または既存 target との衝突により skip となる場合、THEN システム SHALL 衝突種別、共通の target path、および同じPlan内で衝突する全 item の source path と item ID、または衝突する既存 target pathを、当該Planの診断情報として保存する。

WHEN 利用者が衝突 item を確認する場合、THEN システム SHALL「どのsourceとどのsourceが同じtargetになるか」または「どのsourceとどの既存targetが衝突するか」を同じ画面で識別可能に表示し、衝突相手を検索や目視で推測することを要求してはならない。

WHEN 衝突 item が一覧内に単独で表示される場合、THEN システム SHALL 展開操作をしなくても、対象source、少なくとも1件の衝突相手、および共通targetをラベル付きで表示する。同一Plan内の相手が複数ある場合は総件数と全件表示操作を併記し、既存targetとの衝突ではその既存ファイルを衝突相手として表示する。

WHEN 衝突するpathを表示する場合、THEN システム SHALL ファイル名を判別しやすく表示するとともに、完全なpathを確認・コピー可能にする。

WHEN 利用者が重複処理を設定する場合、THEN システム SHALL `skip`、安定した`sequence`、templateによるsuffixを明示的に選択可能にし、選択結果をrules snapshotへ保存する。

### REQ-PLN-004: 手動 target 指定

WHEN 利用者が plan item の target を指定する場合、THEN システム SHALL 元の completed plan を変更せず、指定内容・根拠・親 plan ID を記録した新しい immutable plan を作成する。新 target は通常の path policy、重複検査、snapshot hash 検証の対象とする。

### REQ-PLN-005: 有界かつ決定的なPlan生成

WHEN 大量scan snapshotからPlanを生成または改訂する場合、THEN システム SHALL cursor/batch読取、SQLite staging/index、および設定可能なbounded queueを用い、全scan item、全targetまたは全conflict groupを同時にprocess memoryへ保持してはならない。

WHEN duplicate/conflictを全件判定する場合、THEN システム SHALL lossless path comparison keyとstable ordinalをstaging tableで集約し、巨大な単一conflict groupもpagedに処理する。

WHEN Planのbatch size、worker数またはpage境界が変わる場合、THEN システム SHALL 同一scan snapshot/rulesから同一ordinal、target、eligibility、warning、conflict membershipおよびsnapshot hashを生成する。

IF Plan生成がcancel、失敗またはresource limit到達で中断した場合、THEN システム SHALL runを終端化し、未完成Planをapply不能に保ち、staging dataを安全に再開またはbounded cleanupできるようにする。件数を黙ってtruncateしてcompletedにしてはならない。

### REQ-MDA-001: album artist

WHEN 音声 metadata を読む場合、THEN システム SHALL 対応形式の album artist を取得し、取得不能時だけ artist へのフォールバックを許可する。

### REQ-MDA-002: metadata 不足時の移動

WHEN 利用者が metadata 不足を許可するオプションを有効にし、artistまたはalbumが不足した音楽itemをplanする場合、THEN システム SHALL 不足するartistを `Unknown Artist`、不足するalbumを `Unknown Album` で補完して通常の命名規則からtargetを生成し、安全なtargetを生成できたitemを`eligible`として`metadata_missing` warningと不足理由を保存する。

WHEN 利用者が metadata 不足を許可するオプションを有効にし、metadata全体を読み取れない音楽itemをplanする場合、THEN システム SHALL artistを `Unknown Artist`、albumを `Unknown Album` で補完し、元ファイル名を保持してtargetを生成し、安全なtargetを生成できたitemを`eligible`として`metadata_missing` warningと理由を保存する。

IF metadata 不足を許可するオプションが無効で、artist、album、またはmetadata全体が不足する場合、THEN システム SHALL 当該itemを`blocked`かつ`metadata_missing` blocking reason付きのskipとし、targetを生成してはならない。

WHEN metadata 不足を許可する設定を保存する場合、THEN システム SHALL 当該設定を命名規則およびPlanのrules snapshotへ保存し、設定が存在しない既存データでは安全側の無効を既定値とする。

IF metadata不足時に生成したtargetが既存target、重複target、sourceと同一、またはWindows path policy違反となる場合、THEN システム SHALL 当該itemをskipして理由を保存し、既存targetを上書きしてはならない。

### REQ-AST-001: 同梱画像

WHEN scan が音楽ファイルと同じ source tree 内の対応画像を検出した場合、THEN システム SHALL 画像を scan snapshot に保存し、plan で対応する音楽の target directory へ移動予定を作成する。対応先がない、または一意に決定できない画像は skip と理由を記録する。

IF 画像の移動先候補が複数であり、すべての候補が命名規則から生成された同一album directory直下のdisc directoryである場合、THEN システム SHALL 画像の移動先を当該album directoryとして一意に決定する。sourceのdisc directory内に画像があり、最も近い対応音楽が単一discだけに属する場合は、当該disc directoryを移動先とする。

IF disc directoryの正規化後も画像の移動先候補が複数ある場合、THEN システム SHALL 候補となる全target directoryと根拠となる音楽itemをPlan診断として保存し、移動先を空欄ではなく「未決定（候補N件）」と表示して候補を確認可能にする。

WHEN 利用者が画像の移動先候補を選択する場合、THEN システム SHALL 元Planを変更せず、選択したdirectoryと画像ファイル名からtargetを作るimmutableな改訂Planを生成し、通常のpath policyと衝突検査を適用する。

### REQ-AST-002: 画像名

WHEN 同梱画像の target を作成する場合、THEN システム SHALL source image filename を保持する設定を提供する。保持時に target が重複する場合は `_2` から始まる決定的な連番を付け、既存 target を上書きしてはならない。

### REQ-APL-001: 異ボリューム適用

WHEN source と target が異ボリュームの場合、THEN システム SHALL target directory内にattempt固有のstaging fileを原子的`create_new`で作成し、`copy -> content fingerprint verify -> durable flush -> atomic no-replace publish -> published content verify -> source delete`の順で直列実行する。

WHEN source と target が同一ボリュームの場合、THEN システム SHALL Windows filesystemのatomic no-replace rename/moveを用い、上書きを許すrename APIまたは`exists`確認後の通常renameを使用してはならない。

WHEN staging fileをfinal targetへpublishする場合、THEN システム SHALL publish時点のfilesystem primitiveで「targetが存在すれば失敗」を保証し、親directoryを含む必要なdurability結果と実際のtarget identityをjournalへ保存する。

IF copy、flush、publish後検証またはsource deleteのいずれかが失敗した場合、THEN システム SHALL sourceを可能な限り保持し、sourceとtargetの実在状態をtyped partial resultとして記録し、未検証copyを唯一のcopyとして扱わない。

### REQ-APL-002: Source identity と content verification

WHEN applyが各itemの最初のmutationを準備する場合、THEN システム SHALL 現在のsourceのabsolute path、size、mtime、利用可能なvolume/file identity、reparse stateをplanが参照するscan snapshotと比較し、一致しない場合は新しいscan/planを要求して当該itemを`blocked`にする。

WHEN contentを移動する場合、THEN システム SHALL mutation前のsourceについてversioned fingerprint algorithmで全contentのfingerprintを取得してwrite-ahead journalへ保存し、publish後のtargetを同じalgorithmで検証する。

IF sourceをdeleteする操作が予定される場合、THEN システム SHALL sourceとpublished targetのbyte-equivalenceをfull-content fingerprintまたは同等以上の完全比較で証明し、sizeまたはmtimeの一致だけをsource deleteの根拠にしてはならない。

IF fingerprint読取中にsource identityが変化する、algorithm/versionが一致しない、または完全性を証明できない場合、THEN システム SHALL sourceを削除せずattemptをfailed/recovery_requiredとして記録する。

### REQ-APL-003: Apply/Rollback のreparse防御

WHEN apply、rollbackまたはrecoveryがsource、target、staging pathへアクセスする場合、THEN システム SHALL 各path componentと既存の最近接ancestorをno-follow semanticsで直前に検査し、Plan/operation journalにないreparse pointの出現、identity変更またはroot外への解決を検出する。

IF reparse追跡がrules snapshotで明示opt-inされていない場合、THEN システム SHALL source/target chain上のreparse pointを`blocked`として扱い、link解決先のcopy、renameまたはdeleteを行ってはならない。

IF reparse追跡が明示opt-inされている場合、THEN システム SHALL scan時に保存したlink identity/解決先と現在値が一致し、解決後のsource/targetが許可root内に留まり、循環しない場合だけ操作を続行する。

WHEN rollbackを行う場合、THEN システム SHALL applyと同等以上のreparse/confinement検査を再実行し、過去に許可されたpathであることだけを現在のdelete/restore認可に使用してはならない。

### REQ-VRF-001: 検証

WHEN verifyを開始する場合、THEN システム SHALL 呼出側にtypedな`VerifySubject`のkindとattempt IDを要求し、`ApplyAttempt`、`RollbackAttempt`または将来のsubject kindを文字列や「最新run」の推測で取り違えてはならない。

WHEN `VerifySubject::ApplyAttempt`を検証する場合、THEN システム SHALL 選択したapply attemptのoperation journalを根拠に、各phaseに応じたsource/target/stagingの存在、identity、sizeおよびversioned content fingerprintを検査する。

WHEN `VerifySubject::RollbackAttempt`を検証する場合、THEN システム SHALL 選択したrollback attemptのjournalを根拠に、復元sourceの存在とcontent一致、およびtarget deleteが記録されたitemについてtarget不在を検査し、元applyの期待状態をそのまま使用してはならない。

WHEN verifyを完了する場合、THEN システム SHALL subject kind/ID、verify attempt ID、item/sequence、期待状態、実観測、algorithm/version、検証時刻およびresultを保存し、filesystemを変更してはならない。

IF subjectが存在しない、未完了journalと矛盾する、または別attemptのlogしか存在しない場合、THEN システム SHALL verifyを開始せずtyped errorを返す。

### REQ-RBK-001: 巻き戻し

WHEN rollback を開始する場合、THEN システム SHALL 利用者が選択したexact apply attemptでjournal上効果が証明されたoperationだけを逆sequenceで処理し、別attemptの成功logや集約summaryを混在させてはならない。

WHEN rollbackで異ボリュームのtargetをsourceへ復元する場合、THEN システム SHALL source directory内のattempt固有stagingへ`create_new`し、`copy -> content fingerprint verify -> durable flush -> atomic no-replace publish -> restored content verify -> target delete`をwrite-ahead journal下で実行する。

WHEN rollbackで同一ボリュームのtargetをsourceへ戻す場合、THEN システム SHALL atomic no-replace rename/moveを用い、既存sourceを上書きしてはならない。

IF rollback対象のtargetがapply時に記録したsizeまたは利用可能なfingerprintと一致しない場合、THEN システム SHALL 当該itemをfailedとして記録し、sourceへの復元、targetの削除、上書きを行わない。

IF rollback先sourceが既に存在する場合、THEN システム SHALL journalの期待identity/contentとの同一性を検証し、同一性と過去phaseからalready-restored状態を証明できる場合だけidempotent resultを記録し、それ以外はsource/targetの双方を保持してconflictとする。

IF target delete前に復元sourceのpublish、durabilityまたはcontent一致を証明できない場合、THEN システム SHALL targetを削除しない。

### REQ-ERR-001: 安定した内部契約

WHEN Core、adapter、UIがrun状態、操作結果、理由、またはエラーを受け渡す場合、THEN システム SHALL Coreでは型付けした値を使用し、SQLite・CLI・Tauri境界だけで安定した文字列表現へ変換する。

WHEN Core、adapter、UIがeligibility、warning、journal phase、recovery state、attemptまたは`VerifySubject`を受け渡す場合、THEN システム SHALL exhaustiveなtyped valueとversioned boundary contractを使用し、未知値を成功、eligibleまたはcompletedへfallbackしてはならない。

WHEN Coreがfilesystem pathを扱う場合、THEN システム SHALL native path value objectとroot-bound `SafeTargetPath`を使用し、UTF-8 `String`やdisplay pathを業務identityとして要求してはならない。

IF metadata cacheの読出しとmetadata解析が失敗した場合、THEN システム SHALL cache障害、解析不能、metadata不足を区別し、永続化障害をmetadata不足として握り潰してはならない。

### REQ-OBS-001: 永続化と計測

WHEN run が進行・完了・中断する場合、THEN システム SHALL 状態、件数、警告、ログ、検証結果、および enumerate/tag_read/db_write/plan/apply/verify の duration を SQLite に保存する。

WHEN Desktop アプリを起動する場合、THEN システム SHALL OS のユーザー別ローカルアプリデータ領域に状態 DB の親ディレクトリを作成し、固定ファイル名の SQLite DB を自動的に使用する。利用者に DB path の入力を要求してはならない。

### REQ-OBS-002: Attempt別の順序付き監査log

WHEN apply、verify、rollbackまたはrecoveryを開始・再試行する場合、THEN システム SHALL 論理runとは別の一意なattempt ID、attempt number、parent/predecessor attempt、開始主体、開始/終了時刻および状態を保存し、過去attemptを更新または再利用してはならない。

WHEN item operationまたはjournal phaseを保存する場合、THEN システム SHALL attempt ID内で単調かつ一意なsequence numberを割り当て、item ID、phase、result、identity/fingerprint、error/diagnostic IDと関連付ける。

WHEN retryまたはrecoveryが以前のoperationを再評価する場合、THEN システム SHALL 新しいattemptへ結果を追記し、元attemptのlog、順序またはresultを上書きしてはならない。

WHEN UI/CLIがrun summaryを表示・出力する場合、THEN システム SHALL logical runの集約と選択attemptの集計を区別し、異なるattemptのsuccess/failed件数を単一実績のように合算してはならない。

### REQ-OBS-003: Structured diagnostic とretention

WHEN warning、error、recovery判断またはsecurity拒否を記録する場合、THEN システム SHALL stable code、severity、workflow phase、run/attempt/item/sequence ID、path role、sanitized context、cause chain、message key、発生時刻を持つstructured diagnosticとして保存する。

WHEN diagnosticを利用者へ表示する場合、THEN システム SHALL stable codeから日本語の意味、影響、次の安全な操作へ変換し、内部error文字列だけを表示せず、未知codeにも日本語fallbackとcorrelation IDを提供する。

WHEN diagnosticまたは履歴のretentionを実行する場合、THEN システム SHALL 設定可能な期間/件数policy、依存関係、legal hold相当の保護flag、およびREQ-UI-003のmutation履歴保護を評価し、bounded batch transactionで削除/compactする。retentionは実ファイルを変更してはならない。

IF operation journal、recovery根拠、未rollbackのmutation、未完了attempt、または保護対象との参照関係がある場合、THEN システム SHALL retention対象から除外し、DB容量上限を理由に安全logを黙って破棄してはならない。

WHEN diagnostic exportを作成する場合、THEN システム SHALL schema versionとcorrelation情報を含め、利用者が選択できるpath/metadataのredactionを提供し、secret、OS credentialまたは不要な個人情報を含めてはならない。

### REQ-UI-001: 大量データ UI

WHEN UI が plan または履歴一覧を表示する場合、THEN システム SHALL cursor/page API と仮想スクロールを用い、全 item を WebView へ一括送信しない。

WHEN cursor/page API が末尾を返した場合、THEN UI SHALL 追加読込を終了し、同一 cursor の並行要求、重複 item の追記、および絞り込み後件数を超える表示を防止する。

WHEN UI が plan 一覧を表示する場合、THEN システム SHALL plan 全件数、現在の検索・filterに該当する件数、WebViewへ読込済みの件数、および eligibility/warning/action 別件数を区別して表示する。

WHEN UI が長い source/target path を表示する場合、THEN システム SHALL 一覧全体を横方向へ拡張せず、source、target、理由を識別可能なラベルと省略・展開可能な表示を提供する。

### REQ-UI-002: 操作導線

WHEN 利用者が実行操作を選択する場合、THEN UI SHALL Scan、Plan、Dry-run、Apply、Verify、Rollback を段階表示し、dry-run と本実行を色・文言・確認操作で区別する。

### REQ-UI-003: 履歴整理

WHEN 利用者が履歴を削除する場合、THEN システム SHALL 対象 run と従属する plan、execution、verify、rollback、log を transaction 内で削除し、実ファイルを変更してはならない。実行中 run は削除してはならない。

WHEN 履歴削除またはretentionの候補を評価する場合、THEN システム SHALL run/plan/attempt/journal/verifyの依存closureとfilesystem mutationの現状態をserver側で計算し、`running`、`recovery_required`、未完了journal、未rollbackの成功/partial mutation、rollback後未verify、または他履歴から参照されるrecordをprotectedとして削除してはならない。

WHEN protected履歴を表示する場合、THEN システム SHALL 保護理由、依存するrun/attempt、解除に必要なverify/rollback/export/retention条件を示し、client側の非表示や強制flagだけで保護を迂回させてはならない。

WHEN 削除可能な履歴を削除する場合、THEN システム SHALL 削除される依存closure、件数、期間、および監査上残るtombstoneを事前表示して明示確認を求め、transaction失敗時はclosure全体を保持する。

### REQ-UI-004: 判読可能な実行履歴

WHEN UI が実行履歴を表示する場合、THEN システム SHALL 各 run の種別、状態、開始・終了日時、所要時間、成功・スキップ・失敗または警告の集計を日本語で判読可能に表示し、内部 ID は補助情報として省略表示と全文コピーを提供する。

WHEN 利用者が履歴を探索する場合、THEN システム SHALL run 種別、状態、ID による絞り込み、新旧順の並び替え、および開始日時と ID の安定した複合 cursor pagination を提供する。

WHEN 同じ scan から派生した run が存在する場合、THEN システム SHALL Scan → Plan → Dry-run/Apply → Verify/Rollback の関連をグループとして識別できるようにする。

WHEN 利用者が履歴行を選択する場合、THEN システム SHALL 一覧とは別の詳細領域に完全な ID、親 run、集計、日時、および「この実行を開く」操作を表示し、削除を主操作として表示してはならない。

### REQ-UI-005: Desktop rollback

WHEN 利用者が Desktop から rollback を実行する場合、THEN UI SHALL dry-run と本実行を別の操作として示し、本実行には execution ID、対象件数、不可逆な削除を含む確認操作を要求する。

### REQ-UI-006: Workflow context とstale操作防止

WHEN UIがscan、plan、apply、verify、rollbackまたはrecoveryの結果を受信する場合、THEN システム SHALL backendが返すtyped workflow context（root、scan ID、plan ID、logical run ID、attempt ID、`VerifySubject`、capability、state version）を単一の状態機械へ反映する。

WHEN 利用者がsource root、scanまたはplanを変更・再選択する場合、THEN UI SHALL それより下流のplan/execution/verify/rollback contextと未完了requestを無効化し、古いexecution IDに対するVerify/Rollbackを現在の主操作として残してはならない。

IF 非同期response/eventのcontext IDまたはstate versionが現在contextと一致しない場合、THEN UI SHALL 当該responseをstaleとして表示状態へ混入させず、必要に応じ診断へ記録する。

WHEN mutation操作の可否を表示する場合、THEN UI SHALL backendが最新stateから返すcapabilityと保護理由を根拠にbutton/confirmationを構成し、WebView内のID有無だけでApply、Rollbackまたは削除を有効化してはならない。

WHEN Desktopを再起動する場合、THEN UI SHALL backendから未完了/recovery_required contextを復元し、新しいmutationより先にrecovery状態と安全な選択肢を表示する。

### REQ-UI-007: Accessibility

WHEN Desktop UIのworkflow、virtualized list、dialog、diagnosticまたはprogressを操作する場合、THEN UI SHALL WCAG 2.2 AAを目標にsemantic HTML、論理的なtab順、全機能のkeyboard操作、visible focus、適切なaccessible name/description、およびscreen readerで理解できる見出し・table/list関係を提供する。

WHEN dry-run、本Apply、Rollback、warning、blockedまたはresultを区別する場合、THEN UI SHALL 色だけに依存せずtext、icon/shapeおよびprogrammatic stateを併用する。

WHEN 非同期処理、validation error、dialogまたはworkflow stateが変化する場合、THEN UI SHALL 邪魔にならないlive regionで重要な変化を通知し、dialogのfocus trap/initial focus/return focusとerror箇所への移動を保証する。

WHEN paginationまたはvirtual scrollでfocused itemがunmountされる場合、THEN UI SHALL focusを予測可能なcontainer/隣接itemへ維持し、keyboard/screen reader利用者が現在位置や全件数を失わないようにする。

### REQ-CLI-001: Versioned machine-readable contract

WHEN 利用者がCLIへ`--output json`を指定する場合、THEN CLI SHALL stdoutへschema version、command、status、logical run/attempt/subject ID、counts、typed diagnostics、およびresult/errorを持つversioned JSON envelopeだけを出力し、人間向けmessage、progress barまたはlogを混在させてはならない。

WHEN machine-readable modeでprogress streamingを要求する場合、THEN CLI SHALL 明示したJSON Lines modeで各eventにschema version、event type、sequenceおよびcorrelation IDを付け、terminal resultを一意に識別可能にする。通常のdiagnostic/logはstderrへ分離する。

WHEN CLIがnative Windows pathをJSONへ出力する場合、THEN CLI SHALL readableなdisplay値に加えて非UTF-8/非正規Unicodeも復元できるversioned encodingとpath roleを提供し、lossy display値を再入力用identityとして契約してはならない。

WHEN CLI commandが成功、validation拒否、partial、recovery_required、利用者cancelまたはinternal failureで終了する場合、THEN CLI SHALL 文言に依存しない安定したexit code categoryとtyped error codeを返す。

WHEN JSON schemaを更新する場合、THEN CLI SHALL major/minorの互換規則とmachine-readable schemaを同梱し、同一majorでは既存fieldの意味やenum値を破壊的に変更しない。

WHEN machine-readable modeで破壊的commandを実行する場合、THEN CLI SHALL interactive promptに依存せず、persisted plan/subject IDと明示的execute optionを要求し、不足時はmutation前にtyped validation errorを返す。

### REQ-SEC-001: Desktop/IPC/保存領域の防御

WHEN Desktopをbuildまたは起動する場合、THEN システム SHALL Tauri/WebViewへdefault-denyのContent Security Policyを設定し、local application assetと必要なTauri IPCだけを許可し、`unsafe-eval`、無制限の`unsafe-inline`、任意remote script/frame/connect先を許可してはならない。

IF inline style/scriptがframework上不可避である場合、THEN システム SHALL buildごとのnonce/hashで必要最小限を許可し、CSPを`null`または全許可にしてはならない。

WHEN Tauri commandを公開する場合、THEN システム SHALL allowlistとleast privilege capabilityを用い、typed request size/enum/ID/root bindingをbackendで検証し、WebViewから任意DB path、shell command、filesystem pathまたは未認可URLを渡して権限を拡張できないようにする。

WHEN UIが手動target候補を送信する場合、THEN Desktop backend SHALL persisted Plan root・item・workflow generationに対して候補を検証し、一回限りのopaque capabilityを発行し、改訂実行commandはraw targetを受け取らずbinding済みcapabilityだけをconsumeして、tamper・replay・期限切れ・stale generationを永続化前に拒否する。

WHEN Desktop state DB、journalまたはdiagnostic exportを作成する場合、THEN システム SHALL ユーザー別local app data配下へ安全なACLで作成し、symlink/reparse置換とpath traversalを検査し、secretを平文logへ保存してはならない。

WHEN path、metadataまたはdiagnosticをHTMLへ表示する場合、THEN UI SHALL textとしてescapeし、外部linkは許可scheme/domainを検証して新しいsecurity contextで開く。

### REQ-REL-001: Release integrity と検証gate

WHEN release artifactを作成する場合、THEN システム SHALL clean checkoutと固定されたdependency lock/toolchainからCIでbuildし、Docker正規validation（format、clippy `-D warnings`、workspace test、UI typecheck/build）とWindows固有のpath、reparse、atomic no-replace、crash/recovery、lease、migration、installer testが成功しない限りpublishしてはならない。

WHEN Windows executable/installerをpublishする場合、THEN システム SHALL 信頼されたcode-signing certificateとtimestampで署名し、`longPathAware`等の要求manifestおよびproduction CSPをartifactから検査する。

WHEN releaseをpublishする場合、THEN システム SHALL artifactごとのSHA-256 checksum、version/commit/toolchainを結び付けるprovenance、dependency/licenseを含むSBOM、およびCLI JSON schemaを同時に公開し、CIが生成したartifactを手元buildで置換してはならない。

WHEN installerまたはupdaterがartifactを適用する場合、THEN システム SHALL signature、channel、version monotonicityおよびchecksumを検証し、検証不能なbinaryを実行・置換してはならない。

WHEN 新versionがSQLite schema、plan snapshot、cacheまたはjournal formatを変更する場合、THEN システム SHALL transactional migration、互換性判定、実データcopyによるupgrade testおよび中断recovery testをrelease gateに含め、未知の新versionを旧binaryがmutation可能として開いてはならない。
