# 技術方針

- Rust stable / Cargo workspace、Windows を主要 CI 対象とする。
- Tauri 2、React、TypeScript、Vite。UI は i18n キーを用い、日本語を既定にする。
- SQLite は `rusqlite`、WAL、`busy_timeout`、短い batch transaction、migration を採用する。
- scan の orchestration は Tokio bounded channel、CPU を使うタグ解析は上限付き `spawn_blocking`/worker pool とする。
- CLI は `clap`、エラーは `thiserror`（境界で `anyhow` 可）、観測性は `tracing` と `tracing-subscriber`。
- タグ読取候補は `lofty`。FLAC/MP3/MP4(M4A)/Ogg の統一 API、Rust native、保守性を評価し ADR-001 で確定する。
