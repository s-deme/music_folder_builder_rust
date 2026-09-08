# Contributing

このrepositoryはWindows向けの安全な音楽library整理toolである。既存Python版は参照専用であり変更しない。

## 変更順序

挙動を変える変更は次の順序で行う。

1. `storage/specs/` のEARS要件を更新する。
2. `storage/design/` と必要なADRを更新する。
3. `storage/tasks/` の依存順taskと `storage/design/traceability.ja.md` を更新する。
4. 承認後に実装し、CLIとDesktopで同じCore use caseを使う。
5. 実装状態をtask planと `IMPLEMENTATION_STATUS.ja.md` の両方へ整合させる。

`python scripts/check_traceability.py` はREQ、設計、task、代表testの欠落と状態不整合を検出する。release前は `--require-complete` を付け、未完taskがないことも確認する。negative fixtureの自己試験は `python scripts/test_traceability.py` で実行する。

## 検証

通常の変更では `make validate` を実行する。hostにtoolchainがない場合はREADME記載のDocker Compose等価commandを使う。pull requestではGitHub Actionsが次を再実行する。

- Linux/Windowsのformat、Clippy、workspace test、UI behavior test/typecheck/build/audit
- locked/frozenなCargo dependency graphとnpm lockfile
- RustSecとcargo-denyによるadvisory、license、source policy
- Windowsのlong path、reparse、SQLite migration/fault、CSP、installer smoke
- SDD traceabilityとstatus整合
- CLI JSON schema/exit contractと、3 iterationの10万item Scan/Plan/Apply dry-run/RSS benchmark

dependencyを追加する場合はlockfileを更新し、`deny.toml` のlicense許可を理由なく拡張しない。生成物や`target/`をcommitしない。

`cargo-audit` のinformational warningはCI logでreviewし、workspaceへ直接影響するunmaintained/unsound advisoryは`cargo-deny`で失敗する。一時的なignoreを追加する場合はtracking issue、影響評価、削除条件を同じ変更に記録する。

## Safety review

filesystem mutationではno-replace、source保持、journal、recovery、lease、dry-run parityを壊していないことを示すtestを追加する。Windows path、Unicode、reparse point、既存target、process crashの境界を明示する。security上の問題は公開PRへ詳細を書かず、[SECURITY.md](SECURITY.md)に従う。

## Release変更

release workflowや署名を変更する場合は [release runbook](docs/release/README.ja.md) と [rollback手順](docs/release/ROLLBACK.ja.md) も更新する。本番証明書は実装・pull request・内部unsigned検証に不要であり、保護environment以外へ要求してはならない。
