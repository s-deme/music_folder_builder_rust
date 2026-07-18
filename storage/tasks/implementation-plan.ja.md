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
- [ ] T27: Coreのpath長診断へ対象・実測文字数・上限文字数を追加する。上限以下を許可する境界test、Plan/命名preview/CLI/Desktopに現れる全理由codeの日本語変換と未知codeの日本語fallback test、内部code非表示test、および既存 `path_too_long` riskとの後方互換testを追加する（T02、T13、T18依存）。
- [ ] T28: metadata不足時の方針をNamingRulesへserde default付きで追加し、PlanStoreからscan source rootを取得して、既定では元の相対path・ファイル名を保持したrisk付きmoveを生成する。専用directory/skip方針、root外拒否、Windows path policy、source同一、既存/重複target、旧snapshot互換のCore/SQLite/integration testを追加し、CLI/Desktopの3択設定と日本語理由表示を実装して `make validate` を実行する（T13、T16、T18、T27依存）。
- [x] T29: suffix適用後の同一Plan内衝突を表すgroup/memberをCore・SQLiteへ追加し、Plan作成・改訂時に全相手を永続化する。Plan pageへgroup概要、detail APIへ共通targetと全source/item IDを追加し、Desktopで「衝突相手を表示」・item番号・path copyを実装する。2件以上の衝突、未読込page上の相手、改訂による解消、snapshot整合性を試験する（T14、T16、T18、T25依存）。
- [x] T30: Desktop の状態 DB を Tauri `app_local_data_dir` に自動配置し、親directoryを作成するcommandを追加する。DB path 入力とlocalStorage保存をUIから削除し、path解決後に履歴・workflowを有効化して、起動時の相対path書込み失敗を防ぐ。CLIの明示的な`--db`は維持し、`make validate`を実行する（T03、T10依存）。

## 外部受入確認

- [x] GitHub Actionsをpush/PRで実走し、Windows固有試験とTauri bundle artifactの成功を確認する（run `29350015563`）。
- [ ] 生成されたWindows installerをWindows実機へインストールして起動確認する。
