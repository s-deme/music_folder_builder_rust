# ADR-001: タグ読取に lofty を採用する

## Status

Accepted（2026-07-15）

## Decision

Rust native で主要形式を単一 API から読む `lofty` を第一候補とする。理由は Rust Core への統合、FLAC/MP3/MP4/Ogg の対応範囲、metadata reader port で差替え可能な点である。

## Consequences

自己生成したFLAC/MP3/M4A/OGG fixtureで日本語タグ、欠損タグ、破損入力を自動試験する。readerはport越しに差替え可能なまま維持する。書込み/タグ編集は初期scope外とする。
