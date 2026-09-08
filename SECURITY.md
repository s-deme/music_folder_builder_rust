# セキュリティポリシー

## サポート対象

最新の `production-signed` GitHub Releaseだけをセキュリティ更新対象とする。`internal-unsigned` と `test-signed-not-for-distribution` は検証専用であり、利用者向け配布物ではない。

## 脆弱性の報告

公開Issueへ脆弱性の再現手順、実path、音楽metadata、database、署名資格情報を投稿しない。GitHubのSecurityタブにPrivate vulnerability reportingが表示される場合は、そこから非公開で報告する。利用できない場合は、機密情報を含めず「非公開の連絡経路が必要」とだけIssueで知らせる。

報告には可能な範囲で、影響するversion、OS、攻撃前提、期待結果と実結果、機密化した再現手順を含める。受領後、maintainerは影響範囲と連絡方法を確認し、修正版と公開時期を調整する。

## 秘密情報と配布境界

- 本番PFXとpasswordはGitHubの保護environment `production-signing` のsecretとしてだけ与える。
- 証明書、password、実利用者path、database、診断exportをrepositoryやworkflow artifactへ保存しない。
- CIの自己署名test証明書は一時的に生成・破棄し、配布artifactへ昇格させない。
- production releaseは署名、timestamp、SHA-256、CycloneDX SBOM、GitHub artifact attestationをすべて通過させる。

漏えいの疑いがある場合はreleaseを停止し、資格情報を失効・rotationしたうえで、[rollback手順](docs/release/ROLLBACK.ja.md)に従う。
