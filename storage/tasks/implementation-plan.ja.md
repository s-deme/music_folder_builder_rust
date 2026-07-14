# 実装タスク（完了）

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

## 外部受入確認

- [ ] GitHub Actionsをpush/PRで実走し、Windows固有試験とTauri bundle artifactの成功を確認する。
- [ ] 生成されたWindows installerをWindows実機へインストールして起動確認する。
