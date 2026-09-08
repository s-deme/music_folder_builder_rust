# Contributing

このrepositoryはWindows向けの安全な音楽library整理toolである。既存Python版は参照専用であり変更しない。

通常の作業は新しいbranchを作らず `main` 上で行う。明示的な依頼や作業分離が必要な場合だけbranchを作り、統合後は削除する。Dependabotのversion更新は月次・ecosystem単位でまとめ、同時に開くPRを各1件に抑える。

## 変更順序

挙動を変える変更は次の順序で行う。

1. `storage/specs/` のEARS要件を更新する。
2. `storage/design/` と必要なADRを更新する。
3. `storage/tasks/` の依存順taskと `storage/design/traceability.ja.md` を更新する。
4. 承認後に実装し、CLIとDesktopで同じCore use caseを使う。
5. 実装状態をtask planと `IMPLEMENTATION_STATUS.ja.md` の両方へ整合させる。

`python scripts/check_traceability.py` はREQ、設計、task、代表testの欠落と状態不整合を検出する。release前は `--require-complete` を付け、未完taskがないことも確認する。negative fixtureの自己試験は `python scripts/test_traceability.py` で実行する。

## 検証

通常の変更では `make validate` を実行する。hostにtoolchainがない場合はREADME記載のDocker Compose等価commandを使う。mainへのpushとpull requestではGitHub Actionsが次を再実行する。作業ブランチのpushとPRの二重起動は省く。

- Windowsのformat、Clippy、workspace test、UI behavior test/typecheck/build/audit（各1回）
- locked/frozenなCargo dependency graphとnpm lockfile
- cargo-denyによるRustSec advisory、license、source policy
- Windowsのpath、reparse、SQLite migration/fault、CSP
- SDD traceabilityとstatus整合
- CLI JSON schema/exit contract

installer生成・installer/CLI artifact long-path smokeと3 iterationの1万item Scan/Plan/Apply dry-run/RSS benchmarkは、手動CIの `extended: true` またはReleaseからの呼び出しで実行する。通常CIでは重複する個別Rustテスト、UI typecheck/build、traceabilityの正例検査を再実行しない。`npm --prefix ui test` がtypecheck/buildを、`test_traceability.py` が実repositoryの整合検査を含む。

通常のPlan境界テストは514件で512件の改訂pageを跨ぎ、境界直後の変更と次行の保持を確認する。2万件の負荷試験は既存のignored benchmarkを必要時に実行する。

dependencyを追加する場合はlockfileを更新し、`deny.toml` のlicense許可を理由なく拡張しない。生成物や`target/`をcommitしない。

RustSecの検査は `cargo-deny` に統一する。advisoryの検査範囲は `deny.toml` を維持し、同じdatabaseを検査する `cargo-audit` のインストールと実行を省く。一時的なignoreを追加する場合はtracking issue、影響評価、削除条件を同じ変更に記録する。

## Safety review

filesystem mutationではno-replace、source保持、journal、recovery、lease、dry-run parityを壊していないことを示すtestを追加する。Windows path、Unicode、reparse point、既存target、process crashの境界を明示する。security上の問題は公開PRへ詳細を書かず、[SECURITY.md](SECURITY.md)に従う。

## Release変更

release workflowや署名を変更する場合は [release runbook](docs/release/README.ja.md) と [rollback手順](docs/release/ROLLBACK.ja.md) も更新する。本番証明書は実装・pull request・内部unsigned検証に不要であり、保護environment以外へ要求してはならない。
