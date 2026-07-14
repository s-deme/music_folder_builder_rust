# 構造方針

Cargo workspace を `crates/core`（domain/use case/ports）、`crates/infra`（FS・タグ・SQLite adapter）、`crates/cli`（clap adapter）、`crates/desktop`（Tauri command/event adapter）、`ui`（React/TypeScript）に分割する。

依存方向は `cli/desktop -> core <- infra`。composition root のみが core の port に infra adapter を注入する。UI は Tauri command を通じてページングされた read model と進捗イベントだけを扱い、全 plan item を一括取得しない。
