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
