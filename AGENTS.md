# Music Folder Builder Rust

- 既存Pythonプロジェクトは参照専用で変更しない。CLIとDesktopは crates/core のuse caseを共有する。
- 整理は scan -> plan -> apply -> verify -> rollback とし、apply は永続化済み plan_run だけを入力にする。既存targetは既定で上書きせず、reparse pointを追跡しない。Windowsの禁止文字・予約名・長さ・Unicodeを検証する。
- CoreはTauri・CLI・SQLite・実ファイルI/Oから独立させ、破壊的操作はdry-runと、apply/rollbackの直列・順序付きSQLite操作ログを維持する。
- 実装変更時はDockerの make validate（またはREADMEの等価コマンド）を使う。bin/、obj/、生成物はコミットしない。
