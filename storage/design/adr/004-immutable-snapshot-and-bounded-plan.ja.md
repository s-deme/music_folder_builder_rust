# ADR-004: immutable scan snapshot と bounded Plan build を採用する

- 状態: Accepted
- 日付: 2026-08-26

## 背景

scan itemがpathだけを保持して可変catalogを参照すると、後続scanが過去Planの入力を変える。全itemを`Vec`へ読み込むPlan生成は大規模libraryでメモリ上限を保証できず、page境界の衝突処理も不安定になる。

## 決定

completed scanはlossless source path、identity、size、mtime、content hash、kind、metadata result/versionを自己完結して保持し、更新しない。metadata cacheはreader/schema/config/fingerprintを含むversion keyでappend-only resultを再利用する。

Plan作成・改訂はcursorでsnapshotを読み、build ID付きSQLite stagingへbatch保存する。indexed queryで衝突・画像anchor・suffixを安定ordinal/path-key順に解決し、全targetを再検証する。最終transactionだけがimmutable planをcompletedとして公開し、失敗/cancelしたbuildはapply入力にならない。snapshot hashはversion付きcanonical encoderをCoreで共有する。

## 結果

repository portはpage/streamとstaging primitiveを提供し、業務判断はCoreに残す。性能試験は10万itemでRSS上限、決定性、page境界衝突を確認する。旧snapshotはversion validatorがない限り再Planを要求する。
