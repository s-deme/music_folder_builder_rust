# Windows署名境界

## production input

保護environment `production-signing` に次だけを設定する。

- secret `WINDOWS_SIGNING_PFX_BASE64`: PFXのbase64表現
- secret `WINDOWS_SIGNING_PFX_PASSWORD`: PFX password
- variable `WINDOWS_SIGNING_TIMESTAMP_URL`: RFC 3161 timestamp URL（未設定時はworkflowの既定値）
- variable `WINDOWS_SIGNING_PUBLISHER_SUBJECT`: `Get-AuthenticodeSignature` が返す署名証明書Subjectの完全一致policy

environmentにはrequired reviewerとtag/branch protectionを設定する。値をrepository variable、workflow YAML、artifact、logへ保存しない。hardware-backed署名へ移行する場合も、production signing jobの入力adapterだけを差し替え、unsigned buildを秘密鍵へ接続しない。

## 自動test seam

`scripts/sign_windows_artifacts.ps1 -Mode Test` はCurrentUser certificate storeへ1日有効なcode-signing証明書を生成する。application executableを署名してからTauriでMSI/NSISへpackageし、outer installerも署名する。誤ったPFX passwordと1 byte改変copyが必ず拒否されることを確認し、certificate、PFX、tampered fileを削除する。

test証明書のchainはCI runner内だけで一時的に信頼する。test-signed artifactをproduction releaseへ含めない。production certificateがなくてもこのseamは実行できる。

## production確認

production jobはapplicationを署名後に再packageし、SHA-256 digest、RFC 3161 timestamp、Authenticode policyでMSI/NSISを署名する。launcher PS1にも同じPFXでPowerShell Authenticode署名とtimestampを付ける。最終package runner image、Tauri/Rust/Node/Python、lockfile digest、commitを`windows-installer-package` environment recordへ保存する。続いて `install_verified_release.ps1 -VerifyOnly` が、launcher自己署名、`WINDOWS_SIGNING_PUBLISHER_SUBJECT`、trusted timestamp、manifest/checksum、signed ProductVersionを両installerで検証する。finalize jobはartifact transfer後にもlauncherとinstallerのtrusted同一subjectを確認する。実機受入では組織chain、revocation、SmartScreen表示を別途確認する。
