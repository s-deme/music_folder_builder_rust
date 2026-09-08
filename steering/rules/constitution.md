# Constitution

1. 安全性は性能・利便性より優先する。
2. apply は immutable な保存済み plan を唯一の業務入力とする。
3. dry-run は実ファイルを一切変更しない。
4. target の暗黙 overwrite と、unsafe 状態での唯一のコピー削除を禁止する。
5. scan/plan は非破壊で、run・操作・検証結果を監査可能に保存する。
6. reparse point は明示 opt-in まで追跡しない。
7. Core use case を CLI と GUI が共有し、業務規則を二重実装しない。
8. 大量処理は bounded queue とページングを用い、入力件数比例のメモリ消費を避ける。
9. Windows の Unicode、予約名、禁止文字、長いパス、異ボリュームをテストする。
10. source/target root は lossless な absolute Windows path として保持し、すべての生成・手動 target が保存済み target root 配下に留まることを mutation 前に証明する。
11. execution eligibility と warning を分離し、blocking state は実行せず、明示許可された warning だけを理由に安全な item を暗黙 skip しない。
12. apply/rollback は process 間 mutation lease、write-ahead operation journal、atomic no-replace、source identity と full-content verification の下で実行し、recovery が完了するまで競合 mutation を許可しない。
13. completed scan/plan と全 attempt log は immutable とし、cache 更新で過去 snapshot を変えず、未 rollback・未 recovery の filesystem mutation を証明する履歴を削除しない。
14. apply、rollback、verify、recovery は attempt ごとに順序付きで監査し、verify は typed な subject を明示して rollback 後の状態も検証する。
15. Desktop は default-deny CSP と least privilege IPC を使用し、release artifact は CI gate、署名、checksum、provenance および SBOM で出所と完全性を検証可能にする。
