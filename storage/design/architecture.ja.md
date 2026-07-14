# 構成設計（C4 相当）

## Context

```text
利用者 / 自動化 ── CLI・Tauri Desktop ── Rust Core ── ローカル音楽ライブラリ
                                      └────────────── SQLite 状態 DB
```

## Containers / components

```text
ui (React) -> Tauri commands/events -> desktop -> core use cases <- infra adapters
CLI (clap) ------------------------------^              |          FS / lofty / SQLite
```

`core` は Scan/Plan/Apply/Verify/Rollback use case、domain model、path policy、repository/FS/metadata/progress ports を持つ。`infra` は Windows file walker、metadata reader、SQLite repositories を実装する。desktop/cli は request を組み立て、結果を表示するだけである。

## 実行状態

`scan_run(completed) -> plan_run(completed) -> execution_run(dry_run|apply) -> verify_run -> rollback_run -> verify_run`。apply は `plan_item` の snapshot/hash を検証してから実行し、対象 plan の内容変更を拒否する。apply/rollback は一 worker の順序付き transaction/log flush で処理する。将来の並列 apply は directory lock と独立 target 集合を導入した scheduler に限定する。

apply の実装順序は `plan item検証 -> target存在確認 -> 同一volume rename | 異volume copy -> size検証 -> source delete -> operation log commit` とする。dry-runは同じ事前条件評価とlog保存を行うが、filesystem mutationを呼ばない。

## 互換性・非対象

Python 版と、段階遷移、既定 no-overwrite、reparse point 非追跡、path sanitization、異ボリューム copy-verify-delete、操作ログ、逆順 rollback を互換基準とする。DB は新 schema とし既存 SQLite の直接読取り/移行は初期版対象外。タグの完全な byte-level 同一性、画像/歌詞編集、ネットワークストレージ固有最適化、並列 apply/rollback も対象外。
