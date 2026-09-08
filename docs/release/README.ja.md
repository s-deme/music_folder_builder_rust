# Release runbook

## 信頼channel

| channel | 用途 | 証明書 | 配布可否 |
|---|---|---|---|
| `internal-unsigned` | 実装、installer、SBOM、checksum、attestationの内部検証 | 不要 | 不可 |
| CI test-sign | 署名APIと改変検出の自動試験 | 実行時に一時生成 | 不可 |
| `production-signed` | tagからの利用者向けrelease | 保護environmentの組織証明書 | 全gate成功後のみ可 |

ファイル名に`test-signed-not-for-distribution`を含むartifactは、productionへ昇格または再利用しない。本番署名はunsigned installerのcopyだけを入力とし、build jobへPFXを渡さない。

## 固定したbuild入力

- Rust: `rust-toolchain.toml` のexact version
- Rust dependency: `Cargo.lock` と `--locked` / `--frozen`
- UI dependency: `ui/package-lock.json` と `npm ci`
- Tauri CLI: workflowの `TAURI_CLI_VERSION`
- `cargo-audit`、`cargo-deny`、`cargo-cyclonedx`: workflowのexact version
- GitHub公式Action: Node 24対応versionのfull commit SHAへ固定し、注釈versionをDependabot PRでreview

Cargo cacheはregistry index/cacheとgit databaseだけを対象とする。`target/`、実行binary、監査toolはcacheから復元しない。

`cargo-audit` はlockfile全体の既知脆弱性をfailし、transitiveなinformational advisoryを表示する。`cargo-deny` はこれに加え、workspaceへ直接関係するunmaintained/unsound advisory、license、source policyをfailする。informational warningを黙ってignoreせず、dependency更新時に解消可否をreviewする。

## 自動release手順

1. CI reusable workflowがLinux/Windows validation、dependency policy、traceability、Windows bundle inspectionとinstaller smokeを同じ `${GITHUB_SHA}` で通す。
2. release readinessがstatus整合を常に確認し、production channelだけtask planの全checkbox完了を要求する。internal unsignedは未完taskを隠さずartifact検証を先行できる。
3. CIが生成したMSI/NSISをunsigned policyで検証する。
4. `cargo-cyclonedx 0.5.9` とnpmからRust/UI SBOMを生成する。commit timestampを`SOURCE_DATE_EPOCH`に使う。
5. 自己署名したcopyを検証し、1 byte改変したcopyの署名検証が失敗することを確認する。
6. `production-signed`だけ、保護environment内でapplication executableを署名し、そのbinaryからMSI/NSISを再packageしてouter installerもPFX署名/RFC 3161 timestampする。
7. 保護environmentのexact publisher policyで、両installerのchannel、source identity、単調増加version、checksum、署名、timestamp、ProductVersionを `install_verified_release.ps1 -VerifyOnly` により確認する。
8. installer、SBOM、source commit、channelと、compile/package/SBOM生成runnerを `release-manifest.json` に記録し、`SHA256SUMS` と `INSTALLER-SHA256SUMS` を再検証する。
9. GitHub OIDC/Sigstoreでbuild provenanceとRust/UI SBOM attestationを生成する。
10. tag workflowは既存GitHub Releaseを上書きせず、新規releaseとして公開する。手動workflowはartifactまでで停止する。

tag `v*` は常に`production-signed`であり、production secretsがなければ安全に失敗する。証明書なしでの実装確認はActions画面から `internal-unsigned` を選ぶ。

repository rulesetで `v*` tagの更新・削除を禁止する。workflowは公開直前にもGitHub APIからlightweight/annotated tagをcommitまで解決してevent commitと照合するが、API照合とrelease作成を単一transactionにはできないため、immutable tag rulesetを必須の外部repository設定とする。

## release artifact契約

- MSI installer
- NSIS `*-setup.exe`
- `rust.cdx.json` と `ui.cdx.json`
- `release-manifest.json`（schema version、source commit、channel、size、SHA-256）
- `SHA256SUMS` とinstaller専用 `INSTALLER-SHA256SUMS`
- provenance、Rust SBOM、UI SBOMのSigstore bundle
- `install_verified_release.ps1`（production channel、単調増加version、checksum、署名者、timestampを適用直前に再検証するlauncher）
- `cli-envelope.v1.schema.json`（major 1/minor 1の機械可読CLI契約）
- compile、最終installer package、SBOM生成のbuild environment記録（productionは3 role、internalはpackageを除く2 role）

consumerは最初にchecksumを検証し、次に `gh attestation verify <artifact> --repo <owner/repo>` を使ってrepository identityとprovenanceを確認する。production installerは次のようにexact publisher subjectを指定して検証し、同じlauncherからだけ適用する。

```powershell
./install_verified_release.ps1 -ReleaseDirectory . -InstallerName Music.Folder.Builder_0.1.0_x64_en-US.msi -ExpectedPublisher 'CN=Example Publisher' -VerifyOnly
```

production launcher自体にもinstallerと同じpublisherのPowerShell Authenticode署名とtrusted timestampがあり、実行開始時に自己署名をexact publisher policyで検証する。Windows PowerShellの実行policyが`Restricted`の場合は署名済みscriptも実行できないため、組織管理者が`RemoteSigned`または`AllSigned`を設定した端末を配布対象とする。policy bypassを配布手順に使わない。

launcherはWindows uninstall registryから現在versionを取得するため、利用者がversionを申告してdowngrade検査を迂回する入力はない。`internal-unsigned`、同version、downgrade、checksum不一致、署名・timestamp・publisher・launcher自己署名・binary version不一致を実行前に拒否する。`-VerifyOnly`を外した場合も、同じ検証直後に選択したMSI/NSISだけを起動する。

## CLI JSON互換契約

`--output json` はstdoutへ1行だけを出し、`schema_version: 1`、`schema_revision: {major: 1, minor: 1}`、command/result type、correlation、counts、typed diagnostics、`result`/`data` alias、typed errorを常に持つ。major 1のminor更新はroot/common objectへのadditive fieldだけとし、consumerは未知fieldを無視する。削除、rename、意味変更はmajor更新を必要とする。exit codeは0 success、2 usage、3 blocked、4 partial、5 internal/I/O、6 lease busy、7 recovery required、8 verify mismatch、9 cancelledである。Recoveryのpathはdisplay stringをidentityに使わず、version/role/encoding/raw base64を持つlossless envelopeを正とする。

`--events jsonl` はstderrへ `docs/release/cli-event.v1.schema.json` 準拠のeventを出力する。各eventは単調増加sequenceと同一correlation IDを持ち、最後のterminal eventを一意に識別できる。破壊的な本実行は対話TTYの確認または明示的な`--yes`を要求し、`--output json`ではpromptを出さない。

## 手動確認と外部条件

自動workflowの成功は [Windows実機受入](ACCEPTANCE.ja.md) を置き換えない。本番certificate/HSM、組織のtrust chain、SmartScreen reputationも外部条件である。これらが未提供でも、internal unsigned、test certificate、tamper、SBOM、checksum、provenanceの実装検証は完了できる。

異常があれば公開せず、[rollback手順](ROLLBACK.ja.md)を実行する。
