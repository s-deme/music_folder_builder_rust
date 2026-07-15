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

`core` は Scan/Plan/Apply/Verify/Rollback use case、domain model、命名テンプレート/path policy、repository/FS/metadata/progress ports を持つ。`infra` は Windows file walker、metadata reader、SQLite repositories を実装する。desktop/cli は request を組み立て、結果を表示するだけである。

## 実行状態

`scan_run(completed) -> plan_run(completed) -> execution_run(dry_run|apply) -> verify_run -> rollback_run -> verify_run`。apply は `plan_item` の snapshot/hash を検証してから実行し、対象 plan の内容変更を拒否する。apply/rollback は一 worker の順序付き transaction/log flush で処理する。将来の並列 apply は directory lock と独立 target 集合を導入した scheduler に限定する。

apply の実装順序は `plan item検証 -> target存在確認 -> 同一volume rename | 異volume copy -> size検証 -> source delete -> operation log commit` とする。dry-runは同じ事前条件評価とlog保存を行うが、filesystem mutationを呼ばない。

## 命名・asset・plan revision

Plan は naming rules snapshot（artist/album/disc/filename/duplicate suffix、元音楽・画像ファイル名の保持設定）を保存する。テンプレート展開は Core の純粋関数とし、数値書式と `[{field}]` 形式の条件ブロックを解釈してから component sanitization を行う。音楽 target の重複は suffix template を item 固有値で展開し、なお重複する場合は安定した連番を追加する。それでも既存 target または path risk があれば skip する。

scan は音楽と画像 asset を区別して snapshot に保存する。Plan は音楽 item が決定した source-directory-to-target-directory 対応を根拠に jpg/jpeg/png/webp/gif/bmp を対応付ける。画像は対応音楽がない・複数 target に曖昧に対応する場合に skip とし、source image filename を保持する設定では同一 directory 内で `_2` 以降の連番を付ける。

手動 target 指定は completed plan の item を更新しない。Core の `RevisePlanUseCase` が親 plan と変更集合を読み、新 plan と全 item snapshot/hash を生成する。apply は従来どおり新 plan の保存済み item のみを入力とする。

## 互換性・非対象

Python 版と、段階遷移、既定 no-overwrite、reparse point 非追跡、path sanitization、命名テンプレート、同梱画像の移動、異ボリューム copy-verify-delete、操作ログ、逆順 rollback を互換基準とする。DB は新 schema とし既存 SQLite の直接読取り/移行は初期版対象外。タグの完全な byte-level 同一性、画像/歌詞編集、ネットワークストレージ固有最適化、並列 apply/rollback も対象外。
