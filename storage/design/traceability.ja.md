# 要件トレーサビリティ

要件、主要設計、実装タスク、代表的な自動検証の対応を示す。タスクの完了状態は `storage/tasks/implementation-plan.ja.md` を正とする。

| 要件 | 主要設計 | タスク | 代表的な検証 |
|---|---|---|---|
| REQ-SAF-001〜004 | `architecture.ja.md` 実行状態 | T02、T06〜T08 | Core use case unit test、`workflow_integration.rs` |
| REQ-SCN-001〜003 | `scan-pipeline.ja.md` | T04、T05、T11 | scan/cache/cancel integration test、benchmark |
| REQ-PLN-001 | `architecture.ja.md` path policy・longPathAware、`ui.ja.md` Plan item/Naming preview/Long path option | T02、T06、T13、T27、T32 | Core path policy境界unit test、Plan/preview表示test、Windows manifest・long path integration test |
| REQ-PLN-002〜003 | `architecture.ja.md`、`sqlite-schema.ja.md`、`ui.ja.md` Naming/Duplicate/衝突detail | T13、T20〜T22、T29 | Core naming・衝突group unit test、SQLite detail query test、UI typecheck/build |
| REQ-PLN-004 | `architecture.ja.md` plan revision、`sqlite-schema.ja.md` | T14、T16、T18 | `workflow_integration.rs` plan revision test |
| REQ-MDA-001〜002、REQ-AST-001〜002 | `scan-pipeline.ja.md`、`architecture.ja.md` metadata不足/asset・画像候補、`ui.ja.md` Metadata不足/Image destination conflict | T15、T16、T28、T31 | Core strategy unit test、`metadata_fixtures.rs`、画像候補workflow integration test、UI typecheck/build |
| REQ-APL-001、REQ-VRF-001、REQ-RBK-001 | `architecture.ja.md` 実行状態 | T07、T08、T11 | filesystem workflow、cross-volume、reverse rollback test |
| REQ-OBS-001 | `sqlite-schema.ja.md`、`architecture.ja.md` Desktop DB配置 | T03、T05、T09、T30 | repository/metrics integration test、Desktop/UI build、benchmark |
| REQ-UI-001 | `ui.ja.md` Plan list | T10、T23、T25、T26 | repository cursor test、UI typecheck/build |
| REQ-UI-002 | `ui.ja.md` Workflow | T10、T18 | Desktop command/UI build、workflow integration test |
| REQ-UI-003〜004 | `ui.ja.md` Logs/history、`sqlite-schema.ja.md` | T14、T18、T19 | history repository integration test、UI typecheck/build |
| REQ-UI-005 | `ui.ja.md` Workflow | T18 | rollback Core test、UI typecheck/build |

## 未完了範囲

- T17: CLIのTOML naming設定、plan改訂、履歴整理、本rollback確認契約。
- T24: CLI/Desktop境界を含む自動試験の補強と、検証範囲の再確認。
- T28: metadata不足時の `UnknownArtist` / `Unknown_Album` 補完、読取不能時の元ファイル名保持移動、および日本語理由表示。
- Windows installerの実機受入確認。

`make validate` はDocker内のformat、Clippy、workspace test、UI typecheck、UI production build、依存関係auditをまとめて実行する。Windows固有試験とbundle生成はGitHub ActionsのWindows runnerを正とする。
