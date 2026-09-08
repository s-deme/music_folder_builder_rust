# ADR-005: 共通preflight、attempt所有log、VerifySubject を採用する

- 状態: Accepted
- 日付: 2026-08-26

## 背景

dry-runが実filesystemを検査せず成功を返すと本Applyを予測できない。warningとblocking conditionの混同は実行可能itemをskipする。verify/rollback logをexecutionへ直接関連付けると複数attemptが混ざり、rollback後にもapply後の期待状態を誤用する。

## 決定

Coreの `PreflightEngine` をdry-run/apply/rollback/recoveryの全入口で共有し、snapshot、eligibility、source identity/hash、SafeTargetPath、target不存在、reparse、lease/fencingを検査する。diagnostic issueはseverityを持ち、blocking issueだけがmutationを禁止する。

command呼出しごとにimmutable attemptを作り、preflight、operation、verify、rollback、journal、metricをattempt IDへ関連付ける。Verifyは `VerifySubject::{Execution,Rollback,Recovery}` を明示し、それぞれの期待するsource/target状態を検証する。workflow transitionと安定codeはCore enumを正とする。

## 結果

dry-runと本実行は同じ判定結果を返し、本実行だけがmutation段階へ進む。履歴cleanup/retentionは未完了journal、rollback可能性、legal holdを保護する。diagnostic exportはschema versionとredactionを必須にする。
