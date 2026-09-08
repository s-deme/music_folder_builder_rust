# プロダクト方針

Music Folder Builder Rust は、音楽ファイルのタグから整理先を提案し、利用者が確認した計画だけを安全に適用・検証・巻き戻しできる Windows 第一級のデスクトップ/CLI アプリである。

主な利用者は個人の大量音楽ライブラリ管理者。価値の優先順位は、(1) ファイルを失わない安全性、(2) 大量ライブラリの走査速度、(3) 追跡可能性、(4) 分かりやすい日本語 UI である。

対象形式は初期版で FLAC、MP3、M4A、OGG。破損・衝突・root逸脱など安全性を証明できないitemは適用せず、理由を残して確認可能にする。タグ欠損など利用者がrules snapshotで明示許可した状態はwarningとして可視化し、安全なtargetとcontent保持を証明できる場合だけ実行候補にできる。

DesktopとCLIは同じtyped workflow contextとCore use caseを使用する。dry-runは本実行と同じpreconditionをmutation直前まで評価し、apply/rollbackはno-overwrite、write-ahead recoveryおよびattempt別監査によって、異常終了後も唯一のcopyを失わないことを最優先にする。
