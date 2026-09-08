# 技術方針

- Rust stable / Cargo workspace、Windows を主要 CI 対象とする。
- Tauri 2、React、TypeScript、Vite。UI は i18n キーを用い、日本語を既定にする。
- SQLite は `rusqlite`、WAL、`busy_timeout`、短い batch transaction、migration を採用する。
- scan の orchestration は Tokio bounded channel、CPU を使うタグ解析は上限付き `spawn_blocking`/worker pool とする。
- CLI は `clap`、エラーは `thiserror`（境界で `anyhow` 可）、観測性は `tracing` と `tracing-subscriber`。
- タグ読取候補は `lofty`。FLAC/MP3/MP4(M4A)/Ogg の統一 API、Rust native、保守性を評価し ADR-001 で確定する。
- Coreのpath契約は`Path`/`OsString`を基礎に、native path、display path、Windows comparison keyを分離する。SQLite/JSON境界ではUTF-16をlosslessに復元できるversioned encodingを用いる。
- SQLiteにはimmutable scan/plan snapshot、attempt別operation journal、recovery state、mutation lease/fencing tokenを保存する。Plan/conflict処理はstaging tableとcursor/batchで有界化する。
- Windows filesystem adapterはatomic no-replace、`create_new` staging、durable flush、no-follow reparse検査およびversioned full-content fingerprintを提供する。
- CLIのmachine-readable出力はversioned JSON/JSON Lines schemaと安定exit codeを契約とし、人間向けlogをstdoutへ混在させない。
- Tauriはdefault-deny CSP、command/capability allowlist、backend側root/ID検証を使用する。releaseはlock済み依存、Linux/DockerとWindowsのCI gate、code signing、checksum、provenance、SBOMを必須とする。
