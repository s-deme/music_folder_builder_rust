# ADR-002: lossless Windows path と SafeTargetPath を採用する

- 状態: Accepted
- 日付: 2026-08-26

## 背景

表示用文字列へ変換した Windows path は unpaired surrogate を失う可能性があり、文字列 prefix 判定では sibling path、`..`、UNC/device prefix、reparse point を安全に扱えない。命名、重複suffix、画像、手動改訂の各経路で検証規則が分散すると、target root 外を永続化できる。

## 決定

Core に lossless な Windows path value と、検証済み target だけを表す `SafeTargetPath` capability 型を置く。target root は絶対pathとして固定し、完成した relative components を共通policyで検証してから構造的に結合する。`.`、`..`、prefix、separator混入、NUL/ADS、禁止文字、予約名、末尾空白・ピリオド、長さを一つの規則で扱う。suffix適用、画像destination、Plan改訂の後にも必ず再構築する。

SQLite は権威pathを version 付き UTF-16LE BLOB、表示・検索用pathを派生TEXTとして保存する。collision、snapshot、filesystem操作は同じ `WindowsPathKey`/raw pathを使用する。apply/rollback直前にはparentをno-followで検査し、root外へ遷移するreparse pointを拒否する。

## 結果

文字列だけを受け取るadapterやDB rowは実行権限を持たない。deserialize後もpolicyによる再検証が必要になる。cross-platform testにはportableなcomponent検証を、Windows CIにはlossless/reparse/long-path試験を置く。
