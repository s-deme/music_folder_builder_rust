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
- [ ] T17: CLI に TOML naming 設定、plan revision、history cleanup、および本 rollback の明示確認オプションを追加する。
- [x] T18: Tauri command と React UI に命名設定、plan item target 指定、履歴削除確認、本 rollback 確認を追加する。
- [x] T19: 履歴 repository を集計・親子関係・workflow group・filter・sort・複合 cursor 対応にし、React UI を日本語 table、detail panel、ID copy、非主操作の削除へ改善する。
- [x] T20: 命名プリセット、重複方針、Core validation/preview APIと後方互換serdeを追加する（T13依存）。
- [x] T21: Desktop命名JSONをフォーム、token挿入、階層preview、即時error表示へ置換する（T20依存）。
- [x] T22: 命名validation、preview、重複方針のunit/integration testとUI typecheck/buildを追加する（T20、T21依存）。
- [x] T23: Plan一覧の null cursor 末尾判定、追加読込の排他、古い応答の破棄、重複排除を実装する。
- [ ] T24: Core/infra/CLI/Desktop の unit・integration test、Windows fixture/path test、`fmt`/`clippy`/workspace test と CI を更新する（T22依存）。
- [x] T25: Plan cursor APIへ全件・絞り込み・action/risk別件数とnext cursorを追加し、repository integration testを追加する。
- [x] T26: Desktop Plan一覧へ件数summary、件数付きfilter、読込進捗、source/target/reasonの縦型item表示を追加し、長いpathによる横scrollを解消する（T25依存）。
- [x] T27: component単体の80文字制限と `component_too_long` 診断を撤廃し、path全体が240文字以下なら80文字超のファイル名・フォルダ名も許可する境界testへ更新する。path全体の実測値・上限値、日本語理由表示、および既存 `path_too_long` riskとの後方互換を維持する（T02、T13、T18依存）。
- [x] T28: metadata不足時にartistを `UnknownArtist`、albumを `Unknown_Album` で補完し、読取可能なmetadataには通常の命名規則、metadata全体の読取不能時には元ファイル名を使ったrisk付きmoveを生成する。Windows path policy、source同一、既存/重複target、Core/SQLite/integration test、日本語理由表示を実装して `make validate` を実行する（T13、T16、T18、T27依存）。
- [x] T29: suffix適用後の同一Plan内衝突を表すgroup/memberをCore・SQLiteへ追加し、Plan作成・改訂時に全相手を永続化する。Plan pageへgroup概要、detail APIへ共通targetと全source/item IDを追加し、Desktopで「衝突相手を表示」・item番号・path copyを実装する。2件以上の衝突、未読込page上の相手、改訂による解消、snapshot整合性を試験する（T14、T16、T18、T25依存）。
- [x] T30: Desktop の状態 DB を Tauri `app_local_data_dir` に自動配置し、親directoryを作成するcommandを追加する。DB path 入力とlocalStorage保存をUIから削除し、path解決後に履歴・workflowを有効化して、起動時の相対path書込み失敗を防ぐ。CLIの明示的な`--db`は維持し、`make validate`を実行する（T03、T10依存）。
- [x] T31: 画像の移動先候補と根拠音楽itemをPlan conflict診断へ永続化し、Plan一覧で未決定・候補数・全候補詳細を表示する。候補選択からimmutableな改訂Planを生成し、Core/SQLite/Desktop integration testとDocker検証を追加する（T16、T18、T29依存）。
- [ ] T32: `NamingRules.allow_long_paths`をserde default falseで追加し、既定240文字拒否とopt-in許可のCore境界test、Plan snapshot、手動target改訂、CLI flag、Desktop checkbox/warning、長いpath apply失敗時のsource保持・日本語logを実装する。Windows Desktop/CLI artifactへ`longPathAware` manifestを組み込み、Windows CIでmanifestと長いpath integration testを検証し、Docker validationを実行する（T07、T11、T18、T27依存）。
- [ ] T33: DesktopのPlan一覧と実行ログで衝突を診断card表示し、展開前から対象source、先頭の衝突相手、共通target、相手総数をラベル付きで示す。ファイル名を主表示しfull pathの確認・copyを可能にし、複数相手の全件展開、既存target衝突、detail読込中・失敗・再試行を実装する。単独表示、未読込page上の相手、長い同名path、既存targetのUI testとDesktop integration testを追加し、`make validate`を実行する（T18、T29依存）。
- [x] T34: `NamingRules.allow_missing_metadata`をserde default falseで追加し、無効時はmetadata不足itemをrisk付きskip、有効時は不足artist/albumを `Unknown Artist` / `Unknown Album` で補完してtargetを生成する。rules snapshot、CLI flag、Desktop checkbox、Core/SQLite/integration test、既存snapshotの後方互換testを追加し、`make validate`を実行する（T13、T16、T17、T18、T28依存）。
- [x] T35: 同梱画像の複数target候補がすべて同一album directory直下のdisc directoryである場合、命名規則によるdisc階層の生成根拠を使ってalbum directoryへ正規化する。sourceのdisc内画像、異なるalbum、空またはcustomのdisc template、画像target衝突のCore・SQLite integration testを追加し、`make validate`を実行する（T16、T31依存）。
- [x] T36: Coreのexecution/path policy、SQLiteのmigration/row、Reactのmodel/conflict component、integration test supportを責務別moduleへ分割する。typed workflow error/status/result、失敗時run終端化、metadata/cache error区別、apply中間状態、expected size付きrollback前提条件、WindowsPathKey、transactional SQLite migrationを導入し、重複Desktop scan commandを撤去してDocker validationを実行する（T03、T05、T07、T08、T10、T11依存）。
- [ ] T37: Plan確定時とdry-run/apply前検証のsnapshot hashを、mutationを認可する`ordinal`、`source_path`、`target_path`、`action`、`risk`、`reason`の共通形式へ統一し、画像target候補などの診断情報による誤った`plan_snapshot_mismatch`を解消する。画像候補を含むPlanのdry-run/apply、手動target改訂、永続化済みapply入力の改変拒否をCore/SQLite integration testで検証し、内部error codeを日本語表示へ変換して`make validate`を実行する（T07、T16、T31、T36依存）。

## 外部受入確認

- [x] GitHub Actionsをpush/PRで実走し、Windows固有試験とTauri bundle artifactの成功を確認する（run `29350015563`）。
- [ ] 生成されたWindows installerをWindows実機へインストールして起動確認する。
