# Music Folder Builder Rust

Windows 向け音楽ライブラリ整理ツールの後継。既存 Python プロジェクトは参照専用であり、変更してはならない。

## SDD workflow

1. `storage/specs/` の EARS 要件を更新する。
2. `storage/design/` で設計・ADR を更新する。
3. `storage/tasks/` の依存順タスクを更新する。
4. 承認後にだけ実装する。CLI と Desktop は `crates/core` の use case を共有する。

## Non-negotiable safety rules

- ワークフローは `scan -> plan -> apply -> verify -> rollback`。`apply` は永続化済み `plan_run` 以外を入力にしない。
- すべての破壊的操作は dry-run を提供し、既存 target を既定で上書きしない。
- reparse point は既定で追跡しない。Windows パスの禁止文字、予約名、長さ、Unicode を扱う。
- apply と rollback は初期版で直列実行し、操作ログを item ごと・順序付きで SQLite に保存する。
- Core は stdlib と domain 契約を中心にし、Tauri・CLI・SQLite・実ファイル I/O に依存しない。

## Validation

- 実装時は `cargo fmt --check`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace` を実行する。
- Windows 固有の統合テストを CI の Windows runner で実行する。
