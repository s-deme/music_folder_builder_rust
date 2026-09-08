# Music Folder Builder Rust

Windows向けの、安全性を優先した音楽ライブラリ整理ツールです。FLAC、MP3、M4A、OGGを走査し、変更内容を永続化されたPlanとして確認してから適用します。CLIとTauri Desktopは同じRust Coreを使用します。

利用方法、開発手順、現在の配布状態は [日本語README](README.ja.md) を参照してください。

## 安全性の要点

- 基本フローは `scan -> plan -> apply -> verify -> rollback` です。
- `apply` と `rollback` は既定でdry-runです。本当に変更する場合だけ `--execute` と対象IDに完全一致する `--confirm` を指定します。
- 既存targetは既定で上書きせず、実行履歴と復旧情報をSQLiteへ保存します。
- 一般配布前のWindows実機受入と組織証明書による署名確認は未完了です。重要なライブラリでは必ずバックアップとdry-runを併用してください。

## 文書

- [利用・開発ガイド](README.ja.md)
- [実装状況](IMPLEMENTATION_STATUS.ja.md)
- [要件](storage/specs/library-workflow-requirements.ja.md)
- [設計と検証の対応](storage/design/traceability.ja.md)
- [実装計画](storage/tasks/implementation-plan.ja.md)
- [コントリビューション](CONTRIBUTING.md)
- [リリース手順](docs/release/README.ja.md)
