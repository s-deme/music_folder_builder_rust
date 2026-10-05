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

## 音楽ライブラリ診断

整理先や移動Planを作らずに診断できます。音楽ファイルは変更せず、DBに結果とキャッシュを保存します。
保守用の `diagnostics` とは別の入口です。

```powershell
music-folder doctor scan --source <SOURCE> --db <DB>
music-folder doctor show --run-id <DOCTOR_RUN_ID> --db <DB>
music-folder --output json doctor issues --run-id <DOCTOR_RUN_ID> --severity warning --db <DB>
music-folder doctor duplicates --run-id <DOCTOR_RUN_ID> --db <DB>
music-folder doctor albums --run-id <DOCTOR_RUN_ID> --db <DB>
```

タグ欠損、Artist／Albumの表記揺れ、完全重複、アルバム内の番号・属性不一致を候補として表示します。
MP3／FLAC／M4A／OGGの既存fixtureとWAVの最小PCM入力を検証対象にしています。
AAC／OPUSの正常音声・タグfixtureは未検証です。Desktopの「診断」タブでは、診断の開始・取消、保存履歴、Issueの絞り込み、アルバムとジャケットの閲覧ができます。
規則・制約・終了コード・DB互換性は[診断の設計資料](docs/library-doctor-architecture.md)を参照してください。

## Desktop

Desktopは同じscan／Plan／Apply／Verify／Rollbackを画面から操作するWindowsアプリです。実際のファイル変更は、画面上の計画を確認してから実行してください。

## 開発時の検証

CIの実行環境はWindowsのみです。mainへのpush／PRでRust全体のテスト・Clippy・format、UIテスト（型検査と本番ビルドを含む）、依存関係監査、CLI schemaと文書整合性を検証します。作業ブランチはPRで検証し、pushとの二重起動を省きます。Windowsのpath／reparse／上書き防止／復旧テストもworkspace testで一度ずつ実行します。

インストーラー生成・導入試験と1万件×3回の性能検証は、ActionsのCIを手動実行して `extended` を有効にした場合と、Releaseから呼び出した場合に実行します。タグpushはRelease側だけでCIを呼び、二重実行しません。Windows runnerでの性能閾値の適合は拡張CIで確認してください。

ローカルの検証には既存のDocker開発環境を使用できます。`make validate`、またはPowerShellから次の等価コマンドを実行します。

```powershell
docker compose run --rm dev bash -c 'npm --prefix ui ci && npm --prefix ui audit --audit-level=moderate && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && npm --prefix ui test'
```

`npm --prefix ui test` は型検査・テスト用ビルド・本番ビルド・UI/CSPテストを含むため、検証後に `check` と `build` を再実行する必要はありません。DockerではWindows固有テストの代替にはならないため、その確認はWindows CIで行います。
