# Changelog

このprojectの利用者に影響する変更を記録する。形式はKeep a Changelogに基づき、release versionはSemantic Versioningに従う。

## [Unreleased]

### Added

- immutable scan snapshot、source identity、atomic no-replace、write-ahead recoveryを含む安全性hardening。
- Linux/Windowsの必須CI gate、SDD traceability/status checkerとnegative fixture。
- MSI/NSIS smoke、embedded `longPathAware` manifest、production CSP、schema/fault検査。
- pinned Rust dependency audit、CycloneDX SBOM、SHA-256、GitHub provenance attestation。
- unsigned内部artifact、自己署名test seam、保護されたproduction signing jobの分離。
- releaseとrollbackの運用runbook。
- CLI JSON 1.1 envelope schema、lossless Recovery path、exit code contract、3 iteration benchmark regression policy。
- registry-based downgrade防止とpublisher/timestamp/ProductVersionを適用直前に確認するproduction installer launcher。

### Security

- workflowを最小permissions、cancel可能なCI concurrency、job timeout、locked/frozen dependenciesへ移行。
- 本番署名資格情報を`production-signing` environmentだけに限定。
