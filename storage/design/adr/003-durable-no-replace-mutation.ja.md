# ADR-003: write-ahead journal と atomic no-replace mutation を採用する

- 状態: Accepted
- 日付: 2026-08-26

## 背景

`exists` の後に上書き可能なcopy/renameを行う方式は競合挿入に弱い。filesystem変更後に初めてlogを保存すると、process停止やDB失敗時に変更済みだが監査不能な状態が残る。in-process mutexだけではCLIとDesktopを排他できない。

## 決定

apply/rollback/recoveryはserial workerで、itemごとに `prepared -> staging -> staged -> content_verified -> published -> source_deleted -> completed` を永続化する。`prepared` はfilesystem変更前にcommitする。target directory内の一時fileをexclusive createし、copy中とcopy後にsource identity/content hashを検証し、flush後にatomic `publish_no_replace` する。publish確認前にsourceを削除しない。同一volumeはhandle-bound identity/content検査後のatomic no-replace renameを使い、journal上でstaging phaseを明示的に省略する。

ApplyとRollbackはdry-run/実行で同じPreflightEngine evidenceを計算し、item単位のpreflight logをDBへcommitした後にだけmutationを開始する。特にRollbackは `rollback` または `duplicate_cleanup` の判定、log commit、逆mutationの順を崩さない。heartbeat、preflight finish、operation/result保存が失敗しても、開始済みpreflightと親attemptは元errorを保持したままbest-effortで `failed` または `recovery_required` へ終端化する。

`publish_no_replace`、atomic rename、またはidentity-bound deleteの成功後はfilesystem commit済み領域とみなす。この後のhash/stat/identity取得やjournal CASが失敗した場合、raw DB/I/O errorを通常failureとして返さず、`published_*` / `recovery_post_commit_*` のstable codeへ正規化し、可能なら同じjournalを `recovery_required` へCASする。CAS自体も失敗した場合でも親attemptは `recovery_required` とし、source/target/temporaryの現状態と相関diagnosticを保持して自動再mutationを止める。

mutationはcanonicalなsource/target root全体のOS lockとSQLite leaseを原子的・安定順に獲得し、heartbeat、expiry、単調増加fencing tokenを全journal遷移で検証する。Windows OS lockは共通lock file上でexact scopeをexclusive、祖先scopeをshared byte-range lockとして保持する。このreader/writer構成により祖先/子孫は排他し、無関係なsiblingは並行でき、DBを跨いだprocess crash時もOSがlockを解放する。named mutexはshared祖先を表現できず、全祖先をmutex化するとsiblingを不必要に直列化するため採用しない。未終端journalは起動時に `recovery_required` として検出するだけにし、明示Recovery use caseがdry-runを提示してからresume/rollback/discardを実行する。判断不能時は既存fileを変更しない。

## 結果

異volumeでは単純renameよりI/O量は増えるが、既存target非上書き、source保持、crash後の決定可能性を優先する。byte-range keyのhash衝突は安全側の過剰排他となる。fault injectionはPrepared/Staging/Staged/ContentVerified/Published/SourceDeletedの全境界、stage disk-full/short-write/permission、publish/delete、filesystem commit後DB failure、heartbeat/preflight finish failureをmatrix化する。同一/包含/sibling rootを別DB・2-processで確認する試験とともにrelease gateへ含める。
