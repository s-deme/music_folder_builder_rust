# Windows実機release受入

これは自動CI後に行う外部受入であり、CIの代替ではない。

## EA01: installerと実filesystem

- 対象Windows versionごとにMSIとNSISのclean install、upgrade、silent install、uninstallを確認する。
- WebView2不足・更新時の案内と起動を確認する。
- 日本語、結合文字、予約名境界、240文字超のopt-in path、UNC利用方針を確認する。
- reparse差替え、既存target競合、disk full、permission failure、process kill後のrecoveryでsourceと既存targetが保持されることを確認する。
- rollback後Verifyがrollback状態を検証し、Apply後状態と混同しないことを確認する。

## EA02: 組織署名と配布経路

- MSI/NSISの署名者、chain、RFC 3161 timestamp、revocationを確認する。
- `SHA256SUMS`、release manifest、GitHub attestationをdownload後に検証する。
- MSI/NSISの両方を `install_verified_release.ps1 -VerifyOnly` で検証し、同version/downgrade、誤publisher、checksum改変、timestamp欠落が適用前に拒否されることを確認する。
- 改変copyがAuthenticode、checksum、attestationの少なくとも各対応検査で拒否されることを確認する。
- 組織の配布channelとSmartScreen表示を確認する。

受入結果にはOS build、WebView2 version、release tag、source commit、artifact SHA-256、実施者、結果を記録する。証明書秘密鍵、実利用者path、音楽metadata、databaseは記録へ添付しない。
