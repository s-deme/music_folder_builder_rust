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

## Python 版機能互換（承認待ち）

- [ ] T13: Core に naming rule value object、テンプレート renderer、安定した duplicate suffix/連番解決を追加し、path policy を通す unit test を追加する。
- [ ] T14: SQLite migration で asset 種別、plan rules snapshot、parent plan、target origin を追加し、plan revision・history cleanup repository を実装する。
- [ ] T15: walker/scan pipeline を asset snapshot 対応にし、Lofty adapter で FLAC/MP3/M4A/OGG の album artist を読み取る。fixture と cache/警告試験を追加する。
- [ ] T16: Core Plan/RevisePlan use case に音楽命名、同梱画像の対応付け、画像名保持、重複解決、手動 target 改訂を実装する。
- [ ] T17: CLI に TOML naming 設定、plan revision、history cleanup、および本 rollback の明示確認オプションを追加する。
- [ ] T18: Tauri command と React UI に命名設定、plan item target 指定、履歴削除確認、本 rollback 確認を追加する。
- [x] T19: 履歴 repository を集計・親子関係・workflow group・filter・sort・複合 cursor 対応にし、React UI を日本語 table、detail panel、ID copy、非主操作の削除へ改善する。
- [x] T19: 命名プリセット、重複方針、Core validation/preview APIと後方互換serdeを追加する（T13依存）。
- [x] T20: Desktop命名JSONをフォーム、token挿入、階層preview、即時error表示へ置換する（T19依存）。
- [x] T21: 命名validation、preview、重複方針のunit/integration testとUI typecheck/buildを追加する（T19、T20依存）。
- [x] T22: Plan一覧の null cursor 末尾判定、追加読込の排他、古い応答の破棄、重複排除を実装する。
- [ ] T22: Core/infra/CLI/Desktop の unit・integration test、Windows fixture/path test、`fmt`/`clippy`/workspace test と CI を更新する（T21依存）。
- [x] T23: Plan cursor APIへ全件・絞り込み・action/risk別件数とnext cursorを追加し、repository integration testを追加する。
- [x] T24: Desktop Plan一覧へ件数summary、件数付きfilter、読込進捗、source/target/reasonの縦型item表示を追加し、長いpathによる横scrollを解消する（T23依存）。

## 外部受入確認

- [x] GitHub Actionsをpush/PRで実走し、Windows固有試験とTauri bundle artifactの成功を確認する（run `29350015563`）。
- [ ] 生成されたWindows installerをWindows実機へインストールして起動確認する。
