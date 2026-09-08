# 実装状況

## 現在の状態

リポジトリ内の実装タスク T01〜T54 はすべて完了しています。自動release gateも完了しています。タスクごとの依存関係、完了条件、代表検証は [`storage/tasks/implementation-plan.ja.md`](storage/tasks/implementation-plan.ja.md) を唯一の正本とします。この文書では同じcheckboxやタスク番号一覧を複製しません。

主な実装済み範囲:

- Cargo workspaceをCore／Infra／CLI／Desktopへ分離し、React + TypeScript UIを接続
- `scan -> plan -> dry-run/apply -> verify -> rollback` とrecovery／history archiveを共通Core use caseで実装
- losslessなWindows path、target confinement、immutable snapshot、共通preflight
- atomic no-replace操作、write-ahead journal、process間root lease、attempt別履歴
- bounded scan／Plan処理、metadata cache、取消、進捗・性能計測
- CLIのversioned JSON／JSON Lines契約と安定exit code
- Desktopのmanaged state、capability allowlist、CSP、accessibility／behavior test
- Linux／Windows CI、dependency policy、traceability、SBOM、checksum、provenance、release metadata

## 外部受入

次の2項目はリポジトリ内の実装完了とは別の、対象artifactと外部資格情報を必要とする受入作業です。

1. **EA01 — Windows実機受入:** installerを対象Windows実機へ導入し、WebView2、Unicode／長いpath、reparse、crash recovery、upgrade／uninstallを確認する。
2. **EA02 — 本番署名・配布受入:** 組織のcode-signing証明書／HSMを使い、timestamp、trust chain、配布経路、SmartScreen表示を確認する。

GitHub Actions run `29350015563` では、その時点のLinux／Windows検証とTauri bundle生成が成功しています。これは現在のHEAD、Windows実機受入、または本番署名の完了証拠ではありません。

## 関連文書

- 要件: [`storage/specs/library-workflow-requirements.ja.md`](storage/specs/library-workflow-requirements.ja.md)
- 設計・試験の対応: [`storage/design/traceability.ja.md`](storage/design/traceability.ja.md)
- タスク正本: [`storage/tasks/implementation-plan.ja.md`](storage/tasks/implementation-plan.ja.md)
- 利用・検証: [`README.ja.md`](README.ja.md)
- リリース手順: [`docs/release/README.ja.md`](docs/release/README.ja.md)
