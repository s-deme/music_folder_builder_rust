# 実装タスク

- [x] T01: workspace、crate境界、CI/Windows toolchain、format/lint/test基盤。
- [x] T02: Core domain、run state、plan snapshot、path policy、portとunit test。
- [x] T03: SQLite migration/schema/repository、WAL、busy timeout、履歴・cursor API。
- [x] T04: Windows walker、reparse非追跡、lofty adapter、自己生成fixture。
- [x] T05: bounded scan pipeline、cache、progress、phase metrics、cancel。
- [x] T06: plan use case、sanitization、conflict/risk、immutable snapshot。
- [x] T07: serial dry-run/apply、cross-volume size verify、operation log、idempotency。
- [x] T08: apply検証とreverse-order rollback。
- [x] T09: clap adapterとbenchmark JSON出力。
- [x] T10: Tauri非同期commands/eventsとReact workflow/dashboard/virtual list/history。
- [x] T11: 実filesystem統合試験、Windows path/reparse試験、benchmark harness。
- [x] T12: ADR Accepted、実装状況、CI・運用ガイド同期。

## Python 版機能互換

- [x] T13: Core に naming rule value object、テンプレート renderer、安定した duplicate suffix/連番解決を追加し、path policy を通す unit test を追加する。
- [x] T14: SQLite migration で asset 種別、plan rules snapshot、parent plan、target origin を追加し、plan revision・history cleanup repository を実装する。
- [x] T15: walker/scan pipeline を asset snapshot 対応にし、Lofty adapter で FLAC/MP3/M4A/OGG の album artist を読み取る。fixture と cache/警告試験を追加する。
- [x] T16: Core Plan/RevisePlan use case に音楽命名、同梱画像の対応付け、画像名保持、重複解決、手動 target 改訂を実装する。
- [x] T17: TOML naming 設定、Plan revision、history archive/purge、recovery、exact confirmation付き rollback をversioned JSON/安定exit codeで提供し、CLI process integration testでDesktopと同じ契約を確認する。
- [x] T18: Tauri command と React UI に命名設定、plan item target 指定、履歴削除確認、本 rollback 確認を追加する。
- [x] T19: 履歴 repository を集計・親子関係・workflow group・filter・sort・複合 cursor 対応にし、React UI を日本語 table、detail panel、ID copy、非主操作の削除へ改善する。
- [x] T20: 命名プリセット、重複方針、Core validation/preview APIと後方互換serdeを追加する（T13依存）。
- [x] T21: Desktop命名JSONをフォーム、token挿入、階層preview、即時error表示へ置換する（T20依存）。
- [x] T22: 命名validation、preview、重複方針のunit/integration testとUI typecheck/buildを追加する（T20、T21依存）。
- [x] T23: Plan一覧の null cursor 末尾判定、追加読込の排他、古い応答の破棄、重複排除を実装する。
- [x] T24: Core/infra/CLI/Desktop/UI の成功・失敗境界試験、Windows path fixture、Docker validation、Linux/Windows CIを整備する。
- [x] T25: Plan cursor APIへ全件・絞り込み・action/risk別件数とnext cursorを追加し、repository integration testを追加する。
- [x] T26: Desktop Plan一覧へ件数summary、件数付きfilter、読込進捗、source/target/reasonの縦型item表示を追加し、長いpathによる横scrollを解消する（T25依存）。
- [x] T27: component単体の80文字制限と `component_too_long` 診断を撤廃し、path全体が240文字以下なら80文字超のファイル名・フォルダ名も許可する境界testへ更新する。path全体の実測値・上限値、日本語理由表示、および既存 `path_too_long` riskとの後方互換を維持する（T02、T13、T18依存）。
- [x] T28: metadata不足時にartistを `UnknownArtist`、albumを `Unknown_Album` で補完し、読取可能なmetadataには通常の命名規則、metadata全体の読取不能時には元ファイル名を使ったrisk付きmoveを生成する。Windows path policy、source同一、既存/重複target、Core/SQLite/integration test、日本語理由表示を実装して `make validate` を実行する（T13、T16、T18、T27依存）。
- [x] T29: suffix適用後の同一Plan内衝突を表すgroup/memberをCore・SQLiteへ追加し、Plan作成・改訂時に全相手を永続化する。Plan pageへgroup概要、detail APIへ共通targetと全source/item IDを追加し、Desktopで「衝突相手を表示」・item番号・path copyを実装する。2件以上の衝突、未読込page上の相手、改訂による解消、snapshot整合性を試験する（T14、T16、T18、T25依存）。
- [x] T30: Desktop の状態 DB を Tauri `app_local_data_dir` に自動配置し、親directoryを作成するcommandを追加する。DB path 入力とlocalStorage保存をUIから削除し、path解決後に履歴・workflowを有効化して、起動時の相対path書込み失敗を防ぐ。CLIの明示的な`--db`は維持し、`make validate`を実行する（T03、T10依存）。
- [x] T31: 画像の移動先候補と根拠音楽itemをPlan conflict診断へ永続化し、Plan一覧で未決定・候補数・全候補詳細を表示する。候補選択からimmutableな改訂Planを生成し、Core/SQLite/Desktop integration testとDocker検証を追加する（T16、T18、T29依存）。
- [x] T32: `allow_long_paths` opt-in、snapshot/手動改訂/CLI/Desktop設定、source保持、日本語診断、CLI/Desktop `longPathAware` manifestとWindows契約試験を実装する。
- [x] T33: 衝突card、未読込detail、長いpath、既存target、失敗/再試行、lossless copyをUI behaviorとTauri command契約試験で確認する。
- [x] T34: `NamingRules.allow_missing_metadata`をserde default falseで追加し、無効時はmetadata不足itemをrisk付きskip、有効時は不足artist/albumを `Unknown Artist` / `Unknown Album` で補完してtargetを生成する。rules snapshot、CLI flag、Desktop checkbox、Core/SQLite/integration test、既存snapshotの後方互換testを追加し、`make validate`を実行する（T13、T16、T17、T18、T28依存）。
- [x] T35: 同梱画像の複数target候補がすべて同一album directory直下のdisc directoryである場合、命名規則によるdisc階層の生成根拠を使ってalbum directoryへ正規化する。sourceのdisc内画像、異なるalbum、空またはcustomのdisc template、画像target衝突のCore・SQLite integration testを追加し、`make validate`を実行する（T16、T31依存）。
- [x] T36: Coreのexecution/path policy、SQLiteのmigration/row、Reactのmodel/conflict component、integration test supportを責務別moduleへ分割する。typed workflow error/status/result、失敗時run終端化、metadata/cache error区別、apply中間状態、expected size付きrollback前提条件、WindowsPathKey、transactional SQLite migrationを導入し、重複Desktop scan commandを撤去してDocker validationを実行する（T03、T05、T07、T08、T10、T11依存）。
- [x] T37: rules/schema、scan snapshot、absolute root、lossless path、source identity、disposition/warningを含む共通versioned snapshot encoderへ統一し、tamper/legacy/画像/手動改訂を検証する。

## 安全性・運用ハードニング

以下の安全性・運用ハードニングは実装・自動検証済みである。T38〜T44は本Applyのリリース阻害条件としてCIで継続検証する。

| 状態 | タスク | 依存 | 実装内容 | 代表検証 | 完了条件 |
|---|---|---|---|---|---|
| [x] | T38: `SafeTargetPath` とtarget confinement | T13、T16、T27、T31 | losslessなWindows絶対path表現をCoreに置き、命名、duplicate suffix、画像候補、手動改訂を含む全targetをroot配下へ閉じ込める。 | `..`、絶対component、UNC/device prefix、予約名、Unicode、suffix後逸脱のtable test | 永続化前の全targetが単一validatorを通り、root外・解釈不能pathを実行可能Planへ保存できない。 |
| [x] | T39: execution eligibilityとriskの分離 | T28、T34、T38 | `risk`を診断、実行可否を型付けしたdispositionとして分離し、metadata許可時のmove、skip、blocking errorを明示する。metadata読取不能とduplicate templateのpanic経路も除去する。 | metadata不足/読取不能×許可/不許可×重複方針のmatrix test、panic regression test | warning付き実行可能itemは方針どおり実行され、禁止itemだけがskipされ、入力組合せでpanicしない。 |
| [x] | T40: immutable scan snapshotとsource identity | T03、T05、T15、T36 | scan itemをrun単位のimmutable snapshotとして保存し、cache keyをversion化する。size、mtime、file ID、必要時fingerprintをApply直前に照合する。 | Scan A後に同pathをScan Bで変更するtest、Apply前差替え/同size改変test、migration test | 古いPlanが後続scanで変化せず、sourceの置換・内容変更をmutation前に拒否できる。 |
| [x] | T41: atomic no-replace filesystem操作 | T07、T11、T36、T38 | `exists`先行判定に依存せず、一時fileへの排他的作成、copy、内容検証、flush、no-replace publish、source削除を実装する。同一volume moveも既存targetを原子的に拒否する。 | 同一/異volumeの競合挿入、publish race、部分copy、disk-full fault test | 競合raceでも既存targetを一度も上書きせず、検証済みpublish前にsourceを削除しない。 |
| [x] | T42: write-ahead operation journalとcrash recovery | T36、T41 | mutation前にintentを保存し、`prepared -> staged -> content_verified -> published -> source_deleted -> completed`をitem順に記録する。起動時に孤立runを`recovery_required`へ遷移し、resume/rollbackを提供する。 | 各遷移直後のprocess kill、filesystem成功後DB失敗、再起動resume/rollback test | crash後も実filesystem状態とjournalから次の安全な操作を一意に決定でき、`running`を放置しない。 |
| [x] | T43: process間mutation lease | T03、T42 | canonicalなsource/target root全体をkeyにSQLite lease、heartbeat、owner token/fencing、Windows共通lock file上のexact-exclusive/ancestor-shared byte-range lockを導入し、CLI/Desktop・別DBをまたぐ重複rootのApply/Rollback/Recoveryを排他する。 | 同一/包含/sibling rootを使う2 process・別DB、lease期限切れ、旧owner遅延書込、強制終了test | 重なるmutation rootのownerが常に1つで、非重複siblingを過剰排他せず、stale ownerが再取得後の状態を書き換えられない。 |
| [x] | T44: 共通preflightとworkflow状態機械 | T37、T39〜T43 | dry-runとApplyで同じsnapshot、source identity、target、reparse、eligibility検査を使う。`VerifySubject`でApply後/rollback後を区別し、許可遷移をCoreへ集約する。 | dry-run/Apply parity matrix、禁止遷移、Apply後/rollback後verify、reparse差替えtest | dry-run成功が同一状態のApply可否を正しく予測し、Verifyが対象状態を誤認せず、全mutation入口が同じpreflightを通る。 |

## 基盤整理・有界処理

| 状態 | タスク | 依存 | 実装内容 | 代表検証 | 完了条件 |
|---|---|---|---|---|---|
| [x] | T45: Plan改訂規則のCore集約 | T14、T16、T37〜T39 | SQLiteに残る改訂、path比較、衝突解決、snapshot生成の業務規則をCoreへ移し、adapterは永続化だけにする。 | create/reviseの同一入力golden test、in-memory/SQLite adapter contract test | Plan作成と改訂が同じpath policy・衝突規則・encoderを使い、infraに業務判断が残らない。 |
| [x] | T46: attempt別workflow logと履歴schema | T03、T08、T42、T44 | verify/rollbackをattempt runとして識別し、各log/resultをattempt IDへ関連付ける。journal・lease・診断のmigration、retention、cleanup保護も追加する。 | verify/rollback複数回、旧schema migration、実行中/recovery中cleanup拒否、retention test | 複数attemptの結果を混同せず再現でき、復旧に必要なjournal/診断がcleanupで失われない。 |
| [x] | T47: bounded Plan stagingと性能上限 | T25、T45、T46 | Plan作成・改訂・snapshot計算をcursorとSQLite stagingで処理し、全件`Vec`保持を除去する。 | 10万itemのRSS/時間benchmark、page境界衝突、同一出力golden test | item数に対してメモリが設定上限内に収まり、pageをまたいでも決定的なPlanを生成する。 |

## CLI・Desktop・品質・配布

| 状態 | タスク | 依存 | 実装内容 | 代表検証 | 完了条件 |
|---|---|---|---|---|---|
| [x] | T48: Desktop workflow ownershipとCSP | T30、T44、T46 | backend managed stateからDB path引数を除き、UIをworkflow reducer/job tokenへ移行する。新Scan/Plan/改訂時に古いexecutionを無効化し、最小権限CSPを設定する。 | stale response、新Plan後の旧Verify/Rollback、command引数改変、CSP build/smoke test | 画面に属さないrunを操作できず、任意DBをWebViewから指定できず、`csp: null`が残らない。 |
| [x] | T49: CLI機能・機械可読契約の完成 | T17、T32、T43〜T47 | TOML設定、Plan改訂、履歴cleanup、復旧状態照会、確認token付き本rollbackを追加し、versioned JSON出力と安定exit codeを全commandへ提供する。 | CLI process golden test、invalid config、確認不足、partial/recovery exit code test | Desktopと同じCore workflowをCLIから利用でき、自動化がstdout文字列解析なしで結果を判定できる。 |
| [x] | T50: Desktop診断・accessibility・behavior test | T33、T48 | 衝突card、workflow reducer、進捗、error、copy、keyboard/focus/ariaをcomponent test化し、Tauri command境界をmock/統合試験する。 | 単独/未読込/長path/既存target、detail失敗再試行、keyboard、stale job test | 主要workflowと診断表示の成功・失敗・取消経路が自動化され、キーボードだけで安全操作を完了できる。 |
| [x] | T51: fault injectionと境界試験 | T24、T40〜T50 | Core/infra/CLI/DesktopへDB busy、disk full、permission、race、crash、reparse、同時実行の決定的fault seamと回帰試験を追加する。 | fault matrix integration test、Windows filesystem test、CLI/Desktop boundary test | 各障害でsource/既存target保護、run終端、journal整合性を自動検証し、未試験のmutation境界がない。 |
| [x] | T52: 性能・診断・保持ポリシー | T47、T51 | 大規模scan/plan/applyの基準値、構造化error context、redaction、診断export、履歴保持量と削除policyを定義・実装する。 | benchmark回帰閾値、redaction snapshot、retention/load test | RSS/throughput退行をCIで検知し、機密pathを不用意に漏らさず復旧可能な診断をexportできる。 |
| [x] | T53: CI release gateとtraceability検査 | T24、T32、T50〜T52 | `make validate`、Windows固有試験、manifest/CSP/schema/fault/performance試験を必須化し、REQ・設計・task・test対応と進捗状態の不整合をCIで検出する。 | Linux/Windows CI、traceability checkerへの欠落fixture、artifact inspection | 全必須job成功時だけrelease可能で、未完taskや要件・test欠落をgreenにできない。 |
| [x] | T54: 配布完全性・署名対応 | T53 | version固定、SBOM、checksum、provenance、installer署名入力、署名なし内部artifactを分離し、release手順とrollback手順を自動化する。 | clean release build、SBOM/checksum照合、test certificate署名・改変検出 | 同一commit由来を検証可能なMSI/NSISとmetadataを生成し、本番証明書だけを外部secretとして差し替えられる。 |

## 外部受入確認

- [x] GitHub Actionsをpush/PRで実走し、Windows固有試験とTauri bundle artifactの成功を確認する（run `29350015563`）。
- [ ] EA01（T53、T54後）: 生成されたWindows installerを対象Windows実機へインストールし、WebView2、Unicode/長いpath、reparse、crash recovery、upgrade/uninstallを受入確認する。これはローカル自動検証の代替ではない。
- [ ] EA02（署名資格情報の提供後）: 組織のcode-signing証明書/HSMでMSI・NSISへ本番署名し、信頼chain、timestamp、配布経路、SmartScreen表示を確認する。資格情報とreputationは外部受入条件であり、T54の署名対応実装とは区別する。
