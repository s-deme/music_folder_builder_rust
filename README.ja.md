# Music Folder Builder Rust

Windows上の音楽ライブラリを、タグから生成した整理先へ安全に移動するためのツールです。対応形式はFLAC、MP3、M4A、OGGです。CLIとTauri Desktopは同じRust Core、Plan、履歴DBを利用します。

## 重要な安全上の注意

- 操作は `scan -> plan -> apply -> verify -> rollback` の順で進めます。
- `apply` と `rollback` は既定でdry-runです。`--execute` を付けるまでファイルを変更しません。
- 本実行には対象IDと完全一致する `--confirm` が必要です。既存targetは既定で上書きしません。
- SQLiteの履歴DBと復旧journalを、処理完了と検証が終わるまで削除しないでください。
- 自動検証は整備されていますが、一般配布用artifactのWindows実機受入と組織証明書による署名確認は未完了です。重要なライブラリでは別媒体のバックアップも用意してください。

## CLIの基本フロー

CLIバイナリ名は `music-folder`、Cargo package名は `music-folder-cli` です。まずヘルプで現在の引数を確認できます。

```powershell
docker compose run --rm dev cargo run -p music-folder-cli -- --help
docker compose run --rm dev cargo run -p music-folder-cli -- man
```

以下の `<SOURCE>`、`<TARGET>`、`<DB>` は、コマンドを実行する環境から参照できる絶対パスに置き換えます。Dockerからホスト上の音楽フォルダを扱う場合は、`docker compose run` の追加volumeとして明示的にマウントしてください。

```powershell
# 1. 読み取り専用のscan snapshotを作成する
docker compose run --rm dev cargo run -p music-folder-cli -- scan --source <SOURCE> --db <DB>

# 2. scan結果のIDを使ってimmutableなPlanを作成する
docker compose run --rm dev cargo run -p music-folder-cli -- plan --scan-run-id <SCAN_RUN_ID> --target <TARGET> --db <DB>

# 3. Planをdry-runする（ファイルは変更しない）
docker compose run --rm dev cargo run -p music-folder-cli -- apply --plan-run-id <PLAN_RUN_ID> --db <DB>

# 4. 内容確認後だけ本Applyを実行する
docker compose run --rm dev cargo run -p music-folder-cli -- apply --plan-run-id <PLAN_RUN_ID> --db <DB> --execute --confirm <PLAN_RUN_ID>

# 5. Apply attemptを検証する
docker compose run --rm dev cargo run -p music-folder-cli -- verify --subject execution --subject-id <EXECUTION_RUN_ID> --db <DB>

# 6. 必要ならrollbackをまずdry-runする
docker compose run --rm dev cargo run -p music-folder-cli -- rollback --execution-run-id <EXECUTION_RUN_ID> --db <DB>
```

本rollbackも `--execute --confirm <EXECUTION_RUN_ID>` を明示した場合だけ実行されます。機械処理ではグローバルオプション `--output json` と、必要に応じて `--events jsonl` を利用できます。

## Desktop

Desktopは同じscan／Plan／Apply／Verify／Rollbackを画面から操作するWindowsアプリです。LinuxのDocker環境ではDesktopを含むコードを検証できますが、Windows配布物の作成と実機確認はWindows環境で行います。

Windows向けbundleはGitHub ActionsのWindows runnerでartifactとして生成します。ローカルのコンパイル確認は `make desktop`、UIだけの開発確認は `npm --prefix ui run dev` を利用できます。後者をホストで実行する場合はNode.jsが必要です。

## Dockerでのビルド

ホストへのRust／Node.js導入は不要です。Docker Desktopを起動し、プロジェクトルートで実行します。

```powershell
docker compose build dev
docker compose run --rm dev bash -c "npm --prefix ui ci && npm --prefix ui run build && cargo build --workspace"
```

## 検証

```powershell
make validate
```

`make validate` はDockerの `dev` コンテナ内でformat、Clippy、workspace test、UI behavior／accessibility test、UI typecheck、UI production build、依存監査を実行します。

ホストに `make` がない場合は次の等価コマンドを使います。

```powershell
docker compose run --rm dev bash -c "npm --prefix ui ci && npm --prefix ui audit --audit-level=moderate && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && npm --prefix ui test && npm --prefix ui run check && npm --prefix ui run build"
```

Windows固有のpath／filesystem試験とTauri bundleはGitHub ActionsのWindows runnerで検証します。ローカルで検証できなかったことと、プロジェクト全体が未検証であることは区別してください。

## 実装状況と正本

リポジトリ内の実装タスクと自動release gateは完了しています。一般配布までに残るのは、対象artifactを使った次の外部受入です。

1. Windows実機でWebView2、Unicode／長いpath、reparse、crash recovery、upgrade／uninstallを確認する（EA01）。
2. 組織のcode-signing証明書／HSMを使い、本番署名、timestamp、trust chain、配布経路、SmartScreen表示を確認する（EA02）。

状態を重複管理しないため、詳細なタスク状態は [`storage/tasks/implementation-plan.ja.md`](storage/tasks/implementation-plan.ja.md) を正とします。要件・設計・代表試験の対応は [`storage/design/traceability.ja.md`](storage/design/traceability.ja.md)、利用者向けの状態要約は [`IMPLEMENTATION_STATUS.ja.md`](IMPLEMENTATION_STATUS.ja.md) を参照してください。

性能測定例:

```powershell
docker compose run --rm dev cargo run -p music-folder-cli -- benchmark --source crates/infra/tests/fixtures --db benchmark.db
```
