# 構成設計（C4 相当）

## Context

```text
利用者 / 自動化 ── CLI・Tauri Desktop ── Rust Core ── ローカル音楽ライブラリ
                                      └────────────── SQLite 状態 DB
```

## Containers / components

```text
ui (React) -> Tauri commands/events -> desktop -> core use cases <- infra adapters
CLI (clap) ------------------------------^              |          FS / lofty / SQLite
```

`core` は Scan/Plan/Apply/Verify/Rollback/Recovery/Archive use case、domain model、命名テンプレート/path policy、repository/FS/metadata/progress/lease ports を持つ。`infra` は Windows file walker、metadata reader、lossless path adapter、no-replace filesystem adapter、SQLite repositories を実装する。desktop/cli は Core の command/query DTO を組み立て、安定した境界表現へ変換するだけであり、Plan改訂、実行可否、path比較、復旧判定を再実装しない。

Desktop の状態 DB は Tauri の `app_local_data_dir` 配下の `music-folder.db` とする。Desktop backend は起動時に一度だけpathを解決して親directoryを作成し、process managed stateとして `Arc<ApplicationService>` を保持する。Tauri commandはDB pathを引数に取らず、WebViewから任意DBを開けない。UIへ返すのは診断用の表示pathだけであり、その値を後続commandの権限根拠にしない。CLI は automation・検証用途の明示的な `--db` を維持するが、同じCore/infra serviceとDB/OS leaseを使用する。

Tauri はcommand/capabilityを必要最小限のallow-listにし、remote URL navigation、任意shell、任意filesystem scopeを許可しない。CSPは少なくとも `default-src 'self'` を基礎に、実際に必要なTauri IPC、同梱asset、local styleだけを列挙する。`unsafe-eval`、remote script/style、ワイルドカード接続先は禁止し、開発時だけ必要なoriginはrelease設定へ持ち込まない。CSPとcapability設定はrelease artifactを対象に自動試験する。

## 実行状態

`scan_run(completed) -> plan_run(completed) -> execution_run(dry_run|apply) -> verify_run -> rollback_run -> verify_run` を基本遷移とし、異常終了時だけ `execution_run(recovery_required) -> recovery_run -> verify_run` へ分岐する。各command呼出しは新しいattemptであり、以前のattemptのstatusやlogを上書きしない。Verifyは `VerifySubject::Execution`、`VerifySubject::Rollback`、`VerifySubject::Recovery` のいずれかを明示し、subjectごとの期待状態を検査する。これによりRollback後Verifyは「targetが存在する」というApply後の期待値を誤用しない。

apply はcompleted planのsnapshot/versionを検証し、対象planの内容変更を拒否する。snapshot hashはfilesystem mutationを認可する `ordinal`、losslessなsource/target path、decision、issue severity/code、source expectation、path policy versionを長さ付きcanonical binary encodingでSHA-256へ入力する。表示用path、画像target候補、衝突相手、翻訳文などの診断情報はhash対象外とする。旧snapshot encodingのPlanはversionごとのvalidatorで検証し、validatorを提供できない版は `legacy_non_executable` として再Planを要求する。

dry-runとapply/rollback/recoveryはCoreの同じ `PreflightEngine` を呼ぶ。共通preflightはplan/subject整合性、recovery未解決状態、lossless pathとtarget confinement、source handleのno-follow open、file identity・size・mtime・SHA-256、target不存在、親directoryのreparse point、filesystemのatomic no-replace能力、lease fencing tokenを検査する。dry-runも実際のfilesystemから同じ検査を行い、単にPlan行を成功扱いしない。applyは時間差を信用せずitem直前に再検査し、open handleとsource identityをstaging完了まで維持する。`risk`は利用者向け注意情報であって実行可否の真偽値ではない。Coreは `PlanDecision::{Move,Skip}` と `IssueSeverity::{Advisory,Blocking}` を分離し、blocking issueだけがmutationを禁止する。

### Durable mutation protocol

apply/rollbackは初期版では一workerでserial実行する。各mutation itemは次のwrite-ahead state machineを使い、state遷移とfencing tokenをSQLite transactionで永続化する。

```text
prepared -> staged -> content_verified -> published -> source_deleted -> completed
    |          |              |              |               |
    +----------+--------------+--------------+---------------+-> failed | recovery_required
```

1. `prepared`: operation ID、attempt ID、sequence、source expectation、safe target、temp path、期待hash、lease fencing tokenをfilesystem変更前にcommitする。
2. `staged`: 同一volumeではdirect rename intentとsource handleを確定する。異volumeではtargetと同じdirectoryにoperation ID由来の一時fileをexclusive `create_new`で作り、sourceからcopyする。
3. `content_verified`: 同一volumeではsource、異volumeでは一時fileのSHA-256をPlan期待値と照合する。異volumeの一時fileをflushし、file metadata/length/hashを保存する。対応filesystemでdirectory flushも行う。
4. `published`: 同一volumeはWindowsのatomic no-replace rename、異volumeはfilesystem portのatomic `publish_no_replace(temp,target)`を一回だけ呼ぶ。事前の`exists`確認は診断用であり排他保証に使用しない。no-replaceを保証できないfilesystemでは実行を拒否する。
5. `source_deleted`: published targetをhandle/hashで再確認した後、同じidentityのsourceだけを削除する。
6. `completed`: 最終状態と監査eventをcommitする。

同一volume renameは置換flagを付けず、targetがraceで出現した場合は失敗させる。rename成功時はsource不存在を確認して `source_deleted` へ進む。異volumeではpublish後もsourceを保持し、targetをhashで再確認してから同じidentityのsourceだけを削除する。rollbackもsource/targetの役割を反転した同じprotocolを用い、rollback attempt自身のjournal/logを持つ。単なる `exists -> copy`、上書き可能なcopy、mutation後に初めてlogを書く実装は禁止する。

起動時は非終端journalや期限切れleaseを読取り専用で検出し、該当executionを `recovery_required` にする。起動処理だけで削除・publishしない。Recovery use caseがsource/temp/targetをidentityとhashで照合し、`resume`、`rollback_published`、`discard_unpublished_temp` のdry-run結果を提示した後、明示実行する。期待状態を一意に判定できない場合は `manual_intervention` とし、既存fileを変更しない。各操作はstateのcompare-and-swapとhash照合により再実行可能にする。

### Mutation lease

filesystem mutationの排他は、SQLite leaseとWindowsの共通lock fileに対するOS byte-range lockの二層で行う。各canonical source/target rootは `WindowsPathKey` で正規化し、包含scopeを除いて安定順に一括獲得する。各exact scopeのbyte rangeはexclusive、その祖先rangeはsharedで保持するため、同一・祖先・子孫rootは衝突し、互いに包含しないsibling rootは共通祖先を共有したまま並行できる。lock fileはDB外の利用者共通位置に置くので、CLI/Desktop、別DB、別processでも同じ範囲を調停する。named mutexで全祖先をexclusive取得する案はsiblingを過剰排他し、exact scopeだけを取得する案は祖先/子孫競合を検出できないため採用しない。

獲得順序は全OS range lock、SQLite `BEGIN IMMEDIATE`内の全scope row、実行の順とし、部分獲得は逆順に解放する。一つのlease/fencing tokenはsource/targetの複数rowを所有し、heartbeatとreleaseは全row数の一致を要求する。全journal遷移は現在token・owner・非期限切れrowを要求するため、停止したprocessが後から書込みを継続できない。OS range lockはprocess crashで自動解放される。非終端journalと重なる通常Apply/Rollbackは拒否し、選択されたoperationの明示Recoveryだけがreconciliation後に新tokenを取得できる。PIDやin-process mutexだけを排他根拠にしない。SHA-256からbyte offsetを導くため理論上の衝突は安全側の過剰排他になり、同時mutationの安全性を弱めない。

Coreの各workflowは `execute_inner` とrun lifecycle終端処理を分離し、開始済みrunを成功時は `completed`、item失敗を含む場合は `partial`、cancel時は `cancelled`、処理・永続化・worker失敗時は `failed`、未解決journalがある場合は `recovery_required` へ必ず更新する。run status、action、result、reason/error、journal state、VerifySubjectはCore enumを正とし、SQLite/Tauri/CLI adapterだけが安定codeとの変換を担当する。

rollbackは対象executionのcompleted/published journal実績を入力とし、逆操作前に現在targetのidentity/hash、復元先sourceの不存在、元source expectationを共通preflightで検証する。不一致時は外部変更の可能性があるためmutationを行わない。operation logの表示文や最後のstatusだけをrollback根拠にしない。

Coreは `domain/{naming,path,plan,execution,verification}` と `usecases/{scan,plan,revise_plan,apply,verify,rollback,recovery,archive}` に分割する。infraのSQLite adapterはmigration、scan、plan staging、execution journal、history/archive repositoryに分割し、各migrationをtransactionで適用する。Desktop backendはapplication serviceとjob registryへ集約し、React UIはworkflow reducer、plan、attempt log、history/recoveryのcomponent/hookへ分割する。

## Windows lossless path と SafeTargetPath

Windows pathの権威表現は `OsStr::encode_wide()` 相当のUTF-16 code unit列とし、SQLiteではversion付きlittle-endian BLOBとして保存する。unpaired surrogateを含め、`to_string_lossy()` やJSON文字列への変換で元pathを失わない。表示・検索用の `display_path` は派生値であり、衝突判定、snapshot、filesystem操作、lease名、identity判定には使用しない。Tauri/CLI JSONでは `{encoding:"windows_utf16le_v1", raw_base64, display}` としてlossless値を運ぶ。

Coreの `LosslessWindowsPath` はprefix/root/componentsを構造化し、`WindowsPathKey` はseparator、`.`、case-insensitive comparison、末尾空白・ピリオド、drive/UNC prefixをversion付き規則で正規化する。比較規則はPlan conflict、SQLite key、preflightで共有する。extended-length prefix (`\\?\`) は検証済みabsolute pathにinfraが付与する実行詳細であり、利用者入力や相対componentとして受け付けない。

`SafeTargetPath` はfieldを非公開にし、次の全条件を満たした `TargetPathPolicy` だけが生成できるcapability型とする。

- target rootをlossless absolute pathとroot identityで固定する。
- naming template、duplicate suffix、画像名、手動指定を含む全componentを同じsanitizerへ通す。
- 空component、absolute/prefix、`.`、`..`、separator混入、NUL、ADSの`:`、Windows禁止文字、予約名、末尾空白・ピリオドを拒否または規則どおり置換する。
- 文字数/long-path policyを適用し、component単位でrootへ結合した後、構造上root配下であることを証明する。文字列prefix比較は使わない。
- parentをhandle相対/no-followで辿り、root外へのreparse point遷移がないことをapply直前にも確認する。

Plan itemはraw targetそのものに加えて `target_root_id`、relative components、`path_policy_version`、`WindowsPathKey`を保存する。DBから読んだ値や手動targetは毎回再構築し、deserializeだけで `SafeTargetPath` を得ない。suffix適用後、画像destination選択後、Plan revision後の完成pathも必ず同じconstructorを通す。

## Immutable scan と bounded Plan build

`scan_items` はcompleted後に変更しないself-contained snapshotである。Planは可変なcatalog行をjoinせず、snapshotに保存されたlossless source path、file identity、size、mtime、content hash、asset kind、metadata result ID/versionだけを読む。metadata cacheはappend-only resultで、`reader_id + reader_version + metadata_schema_version + reader_config_hash + source_fingerprint` が完全一致した場合だけ再利用する。reader更新後に古いタグを暗黙再利用しない。

Plan作成は全件を `Vec` へ読み込まない。Coreのbuilderはcursor/pageでcompleted scan snapshotを読み、SQLiteのbuild ID付きstaging tableへdraft item、anchor、normalized target key、source expectationをbatch保存する。indexed staging queryで重複groupと画像候補を決め、安定したordinal/path key順でsuffixを割り当て、全行を再度SafeTargetPathへ通す。最後のtransactionで `plan_runs(status=building)` とstaging行をimmutable `plan_items`/conflict tablesへ移し、hashを計算して `completed` にする。失敗・cancel時はcompleted planを公開せず、orphan stagingはstartup maintenanceでbuild owner/leaseを確認して削除する。メモリ上限はpage、worker queue、現在処理中groupだけに比例させる。

手動target・画像候補選択・命名規則変更は `RevisePlanUseCase(parent_plan_id, expected_parent_hash, ChangeSet)` だけで処理する。Coreが親Planをstream読取りし、変更を適用して全SafeTargetPath、source expectation、duplicate/conflict、snapshot hashを再評価し、新しいcompleted planを作る。SQLite adapterやDesktopが単一行を直接更新するAPIは提供しない。親planは永久にimmutableで、applyは新planの保存済みitemだけを入力とする。

## 命名・asset・plan revision

Plan は naming rules snapshot（artist/album/disc/filename/duplicate suffix、元音楽・画像ファイル名の保持設定）を保存する。テンプレート展開は Core の純粋関数とし、数値書式と `[{field}]` 形式の条件ブロックを解釈してから component sanitization を行う。音楽 target の重複は suffix template を item 固有値で展開し、なお重複する場合は安定した連番を追加する。それでも既存 target または path risk があれば skip する。

suffix適用後も残る同一Plan内の衝突は、理由codeだけでなくPlan snapshot内の衝突groupとして保存する。groupは安定したID、比較に用いた正規化target path、および同じPlan内の全member item IDを持つ。各memberのsource pathはPlan itemから取得する。Plan item pageはgroup ID・相手件数だけを返し、展開時のdetail queryがgroup全memberのitem ID/source pathと共通targetを返す。これにより大量一覧を肥大化させず、各衝突行から相手を直接確認できる。plan revisionは全itemの衝突groupを再評価し、親Planのgroupを流用しない。既存targetとの衝突はapply/dry-runのitem結果でsourceと既存target pathを併記する。

Desktopの衝突itemは通常のPlan itemとは異なる診断cardとして表示する。cardの初期状態に「対象ファイル」「衝突相手」「共通の移動先」を配置し、それぞれファイル名を主表示、full pathを補助表示・copy対象とする。同一Plan内の衝突ではdetail queryを表示時に取得し、現在itemを除いた先頭の相手を初期表示する。相手が複数なら「ほかN件」と全件展開を提供する。既存targetとの衝突はoperation logのsource/target/error codeから同じ3項目を構成し、単独のlog行でも相手が既存targetだと分かる文言にする。読み込み中や取得失敗を単なる「衝突」表示へ退行させず、状態と再試行手段を示す。

CoreはUIから独立した命名規則validation/preview APIを持つ。validationはtoken構文、field allow-list、必須component、生成後path policyを返す。NamingRulesの追加fieldはserde defaultを持ち、保存済みsnapshotを読み取れる後方互換性を維持する。

`NamingRules.allow_missing_metadata` はserde default `false`とし、CLI/Desktopの明示opt-inとPlan rules snapshotへ保存する。無効時にartist、album、またはmetadata全体が不足する音楽itemは、targetなしの `action=skip`、`risk=metadata_missing` と具体的な不足理由を保存する。有効時はalbum artist/artistのfallback値を `Unknown Artist`、albumのfallback値を `Unknown Album` として通常の命名templateを展開し、読み取れた値はfallbackで置き換えない。metadata全体を読み取れない場合は同じartist/album fallbackを使い、title等に依存する命名を避けて元ファイル名を保持する。生成targetは通常の重複解決、既存target確認、source同一判定、path policyを通し、Plan snapshot確定後に再計算しない。

path policy は既定でtarget path全体を240文字まで許可する。component単体の文字数上限は設けず、ファイル名やフォルダ名が80文字を超えてもpath全体が240文字以下なら許可する。文字数はRustの `chars()` によるUnicode scalar value数で数え、上限値そのものは許可し、上限超過時だけ拒否する。

`NamingRules.allow_long_paths` はserde default `false`とし、CLI/Desktopの明示opt-inとPlan snapshotへ保存する。trueの場合は240文字超を `action=move` として許可し、実測長を監査可能にするが、成功を保証しない。Windows Desktop executableはmanifestへ `longPathAware=true` を埋め込み、Windows 10 1607以降の `LongPathsEnabled=1` と組み合わせる。CLIも同じ設定を受けるが、CLI executable自身のmanifest対応をWindows artifactで検証する。applyは従来どおりfilesystem error時にsourceを削除せず、長いpath itemでは環境条件を示す日本語errorをoperation logへ保存する。既定false時はCoreの実測文字数・上限文字数診断と既存 `path_too_long` risk/filterを維持する。

Plan reasonと命名validation issueは永続化・判定用の安定した内部codeを維持し、表示adapterで全codeを日本語へ変換する。既知codeは具体的な日本語文言とし、未知codeは「詳細不明の理由があります」のような日本語fallbackに、調査・copy用の補助情報を分離して提示する。内部codeそのものを主たる利用者向け理由として表示しない。

scan は音楽と画像 asset を区別して snapshot に保存する。Plan は音楽 item が決定した source-directory-to-target-directory 対応を根拠に jpg/jpeg/png/webp/gif/bmp を対応付ける。対応付けは画像から最も近い音楽を含むsource祖先を使用し、sourceのdisc directory内にある画像はそのdiscだけへ対応させる。画像は対応音楽がない・複数 target に曖昧に対応する場合に skip とし、source image filename を保持する設定では同一 directory 内で `_2` 以降の連番を付ける。

複数の画像target候補がすべて同一album directory直下のdisc directoryである場合は、album directoryを画像targetとして自動選択する。disc directoryかどうかはpath名の形式や単なる共通祖先では判定せず、各音楽itemについて命名規則の `disc_dir_template` が空でないdirectory componentを実際に生成したことをPlan作成中の一時的なanchor情報として保持して判定する。候補の一部にdisc directoryの生成根拠がない場合、または直接の親が異なる場合は正規化しない。この一時情報は永続domain・SQLite schemaへ追加しない。

disc directoryの正規化後も画像の対応先が複数ある場合は、画像itemに `image_destination` conflict groupを割り当て、候補target directoryごとに根拠となる音楽Plan item IDを保存する。Plan pageは候補数を返し、既存のconflict detail APIは種類に応じて候補directoryと音楽itemのordinal/source pathを返す。候補選択は画像ファイル名をcandidate directoryへ結合して既存のPlan revisionへ渡し、親Planを変更しない。

source expectationは、Windowsで取得可能ならvolume serialと128-bit file ID、必ずsize、mtime、SHA-256、identity/hash algorithm versionを持つ。Plan完成までに全move itemのSHA-256を確定し、dry-runは現在sourceを再hashする。applyは同じhandleからcopyしながら再hashし、copy前後のidentity/statも一致した場合だけpublishへ進む。file identityを提供しないfilesystemではhashを必須の同一性根拠とし、identityを取得できない事実もdiagnosticへ残す。

## Attempt、履歴、診断

dry-run、apply、verify、rollback dry-run、本rollback、recoveryは呼出しごとにimmutableなattemptを作る。preflight/log/journal/metricはそのattempt IDを必須外部キーとし、前回attemptの行を更新・流用しない。特に `verify_logs.verify_run_id` と `rollback_logs.rollback_run_id` を所有者とし、同じexecutionに対する複数回の検証・rollbackを時系列で比較できるようにする。集計値はattemptごとのlogから導出し、異なるattemptを混ぜない。

warning、error、recovery判断、security拒否は、元処理の成否を変えないbest-effortなstructured diagnosticとして保存する。各eventはlogical run IDとattempt ID、item/sequence（item eventの場合）、phase、stable code、severity、path role、message key、sanitized cause chain、correlation IDを持つ。payloadはallow-list化し、secret名だけでなくBearer値、`password=...`、URI userinfo等の値patternも保存前とexport時の双方でredactする。表示はstable codeから日本語の意味・影響・次の安全な操作へ変換し、未知codeは内部文字列を出さず日本語fallbackとcorrelation IDを示す。

履歴の既定整理操作はhard deleteではなくworkflow graph単位のarchiveである。archive use caseはactive run、mutation lease、recovery_requiredに加え、journalが示す未rollbackの成功mutationと、completed rollback後に`VerifySubject::Rollback`が成功していないworkflowを拒否する。pre-publishで安全に終端化した`failed` journalだけを永久保護理由にはしない。同じ保護判定をarchive preview、archive、検証済みarchive後のpurgeで共有する。scanから派生するPlan/attempt/journal/log/diagnosticをversion付きmanifestとcanonical JSONLへstream出力し、一時archiveをflushして件数とSHA-256を検証し、atomic no-replace publishした後にだけactive DB側を `archived` とする。容量回収のpurgeは明示設定/確認を要し、検証済みarchive manifestを保持する。archiveは媒体fileを含まず、manifest pathはnative path codec（Windows UTF-16LE code unit、Unix byte列、version付きfallback）でlosslessに保存し、表示文字列から再構成しない。purgeは現行archive schemaで再検証できるmanifestだけを根拠とし、旧schemaしかない場合は現行版を再archiveし、未知の新版は拒否する。

operation journal、lease、archive manifest、verify/rollback auditはworkflowをarchive/purgeするまで削除しない。`event_logs`はclass別retentionを持ち、progress/debugは既定7日、通常diagnosticは30日、failed/recovery関連は180日を削除不可の安全下限とする。最大byte数を超えても、期間未到達、protected、active attempt、未終端journal、recovery根拠を容量都合で削除しない。安全に削除できる行がないover-cap状態は `capacity_exceeded` として返し、削除延期理由・DB byte数・設定上限をretention runと監査eventへ残してoperator対応を要求する。監査classはarchive完了前にpruneしない。payloadにはschema versionとredaction classを付け、タグ全量や不要な秘密情報を保存しない。maintenanceの削除件数・期間・理由自体をeventとして残す。

## CLI と境界契約

CLIは対話表示を既定とし、automationには `--output json` と任意の `--events jsonl` を提供する。JSON stdoutはcommandごとに一つのenvelope `{schema_version, command, status, run_id, result, error}` とし、progressはstderrへ分離する。pathはlossless path envelope、enum/errorは安定code、表示文は別fieldとする。applyはcompleted `plan_id`、rollbackは対象execution ID、recoveryは対象journal/run IDしか受け取らず、ad-hoc source/target一覧を入力にできない。本実行はTTY確認または明示的な `--yes` を要求する。

exit codeは `0=成功`、`2=usage/config`、`3=blocking preflight/plan conflict`、`4=partial item failure`、`5=I/O・DB・internal failure`、`6=lease busy`、`7=recovery required`、`8=verify mismatch`、`9=cancelled` として固定し、localized messageから判定しない。JSON schemaとexit code mappingはgolden/contract testでCLI/DesktopのCore error mappingと照合する。

## Migration とversioning

SQLite migrationは単調増加integer ID、変更不可のname/checksum、適用app versionを持ち、各migrationを `BEGIN IMMEDIATE` transactionで適用する。起動appが対応する最大schemaよりDBが新しい場合はwriteを拒否し、対応範囲外の古いDBはbackup/明示upgradeを要求する。migration前に非終端journal、active lease、running runがないことを確認し、破壊的table rebuild前には同directoryへ検証可能なbackupをatomic作成する。失敗時はtransaction rollbackし、元DBを使用可能なままにする。

DB schemaだけでなく `path_encoding_version`、`windows_path_key_version`、`scan_snapshot_version`、`metadata_schema/reader_version`、`plan_snapshot_encoding_version`、`journal_protocol_version`、`archive_schema_version` を行またはmanifestへ保存する。versionを推測せず、未知versionは安全側に読取/実行拒否する。migration testは空DB、現行DB、サポートする各旧fixture、途中失敗、再実行、newer-than-app拒否、journal/recovery中拒否を含む。

## 互換性・非対象

Python 版と、段階遷移、既定 no-overwrite、reparse point 非追跡、path sanitization、命名テンプレート、同梱画像の移動、copy-verify-delete、操作ログ、逆順 rollback を互換基準とする。ただし安全性を弱める挙動やPython版DB形式をそのまま引き継がない。本Rust版で既に生成したSQLiteは明示したsupport window内でmigrationし、Python版DBの直接upgradeは対象外とする。タグの完全なbyte-level同一性、画像/歌詞編集、ネットワークストレージ固有最適化、並列apply/rollbackも対象外である。
