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

Desktopは同じscan／Plan／Apply／Verify／Rollbackを画面から操作するWindowsアプリです。実際のファイル変更は、画面上の計画を確認してから実行してください。
