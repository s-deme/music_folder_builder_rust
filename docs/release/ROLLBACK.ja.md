# Release rollback手順

## 判断と封じ込め

1. checksum、attestation、署名、installer、データ保全のいずれかに疑義があれば、新規downloadを止める。
2. 問題のtag、workflow run、source commit、release manifest、artifact digest、報告時刻を記録する。利用者pathやdatabaseを公開記録へ含めない。
3. 署名鍵漏えいの疑いがあれば、`production-signing` environmentを無効化し、証明書を失効・rotationする。
4. GitHub Releaseを削除する前にSigstore bundle、SBOM、checksum、監査logをincident用のアクセス制御された場所へ保存する。attestation削除は検証利用者へ影響するため、incident責任者が判断する。

## 配布の巻き戻し

- 問題releaseをdraftへ戻すか削除し、release noteへ利用停止を明記する。tagやassetを同じversionの別binaryで上書きしない。
- 修正版は新しいversion、tag、同一commit gate、署名、SBOM、checksum、provenanceをすべて再生成する。
- 復旧版もRelease経由の `extended: true` CIでWindows installerと性能検証を通す。通常push／PRのCI成功だけを配布判定に使わない。
- 単純なdowngradeはversion monotonicityと新schema互換性を破るため配布しない。直前の実装へ戻す必要がある場合も、修正を新しいversionとしてbuildし、そのchecksum、attestation、Authenticode署名、migration互換性を再検証する。
- download mirrorや社内配布cacheがある場合はdigest単位で停止し、置換ではなく新versionを配布する。
- 復旧版も `install_verified_release.ps1` のregistry-based単調増加versionとexact publisher policyを通す。旧版を直接起動してlauncherを迂回しない。

## application/data rollback

applicationの前進versionによる復旧とlibrary workflow rollbackは分ける。SQLite schemaを古いbinaryへ直接downgradeしない。音楽fileの復旧はapplicationのjournal/recovery/rollback use caseだけを使い、Explorerやscriptで途中状態を推測して一括移動しない。

crashや部分Applyが関係する場合はmutation leaseを確保し、`recovery_required` run、operation journal、source/target identityを保全してからresumeまたはrollbackを選ぶ。診断保持policyを回避して履歴を削除しない。

## 復旧完了条件

- 問題artifactへの新規導線がない。
- 影響version、digest、期間、利用者対応が記録されている。
- 必要な鍵失効・rotationが完了している。
- 既知良好版または修正版のchecksum、attestation、署名、Windows実機受入が完了している。
- 原因と再発防止をEARS、設計、task、testへ反映した。
