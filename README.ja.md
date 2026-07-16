# Music Folder Builder Rust

## Dockerでのビルド

ホストへのRust/Node.js導入は不要。Docker Desktopを起動して、プロジェクトルートで実行する。

```powershell
docker compose build dev
docker compose run --rm dev bash -c "npm --prefix ui ci && npm --prefix ui run build && cargo build --workspace"
```

検証は次の通り。

```powershell
make validate
```

`make validate` は Docker の `dev` コンテナ内で Rust と Node.js を用いて、format、Clippy、workspace test、UI typecheck、UI production build を一括で実行する。WSL ホストに Rust/Node.js を導入する必要はない。

ホストに `make` がない場合は、同じ検証を次で実行する。

```powershell
docker compose run --rm dev bash -c "npm --prefix ui ci && npm --prefix ui audit --audit-level=moderate && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && npm --prefix ui run check && npm --prefix ui run build"
```

LinuxコンテナではCore/CLI/Desktop/UIを検証する。GitHub ActionsのWindows runnerはWindows固有試験の後にTauri bundleを作成し、artifactとして保存する。

## 現在の実装範囲

CLIとDesktopは同じRust Coreを使い、`scan -> plan -> dry-run/apply -> verify -> rollback`を実装済み。apply/rollbackは直列で、永続化済みplan/operation logのみを根拠にする。Desktop scanは非同期開始・状態照会・取消、100ms進捗通知、速度・ETA表示に対応する。

要件、設計、実装タスク、主な検証の対応は [`storage/design/traceability.ja.md`](storage/design/traceability.ja.md) を参照する。実装済み範囲と残作業は [`IMPLEMENTATION_STATUS.ja.md`](IMPLEMENTATION_STATUS.ja.md) にまとめる。

性能測定例：

```powershell
docker compose run --rm dev cargo run -p music-folder-cli -- benchmark --source crates/infra/tests/fixtures --db benchmark.db
```
