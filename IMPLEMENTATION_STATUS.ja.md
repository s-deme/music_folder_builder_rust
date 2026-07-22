# 実装状況

## 完了済み

- Cargo workspaceをCore / Infra / CLI / Desktopへ分離し、React + TypeScript UIを接続。
- `scan -> plan -> dry-run/apply -> verify -> rollback`を同一Core use caseで実装。
- 永続化済みplan snapshot hash、target再確認、dry-run非変更、成功itemの冪等apply。
- apply/rollbackの直列実行、item単位の順序付きログ、逆順rollback。
- 異ボリュームのcopy→size検証→source削除と、失敗時source保持。
- verifyのtarget存在・source不在・apply時期待サイズ検証。
- SQLite WAL、busy timeout、batch transaction、migration、run/log/verify/rollback/metrics履歴。
- bounded scan pipeline、設定可能worker、metadata cache、取消時writer drain/commit、cancelled状態。
- Desktopの非同期scan開始・状態照会・取消、100ms進捗間引き、件/秒・ETA。
- Desktopの状態DBをユーザー別ローカルアプリデータ領域へ自動配置し、UIからDB path設定を除外。
- plan/operation logのkeyset cursor検索・filterと、可変行高virtualizer。
- 命名テンプレート、プリセット、重複方針、validation/preview、保存済みrules snapshot。
- album artist読取、同梱画像asset、元ファイル名保持と決定的な重複解決。
- immutableなplan改訂、Desktopでのtarget指定、履歴削除確認、本rollback確認。
- plan一覧の末尾判定・要求排他・古い応答破棄・重複排除と、全件・絞り込み・action/risk別件数表示。
- 実行履歴の親子関係・workflow group・複合cursor・日本語table・detail panel。
- 日本語辞書、ライト/ダーク/システムthemeの永続化、Apply確認画面。
- 自己生成・無音・CC0-1.0のFLAC/MP3/M4A/OGG fixture、日本語/欠損/破損/衝突入力。
- 実filesystemの全workflow、warm cache、snapshot不一致、冪等性、取消、cross-volume失敗、逆順rollback、cursor/metrics/history試験。
- cold/warm時間、件/秒、cache hit率、phase metrics、RSSを出力するbenchmark。
- Linux/Windows CI、Windows固有Unicode/予約名/長いpath/reparse試験、Tauri bundle artifact定義。
- GitHub Actions run `29350015563`でLinux/Windows検証とTauri bundle生成が成功。
- Windows artifact内のMSIおよびNSIS setup executableの生成・ダウンロード確認。
- workflow error・run status・operation resultをCoreで型付けし、開始済みrunの失敗終端処理を追加。
- applyのcopy後source残存を独立した操作状態として保存し、rollback前にexpected sizeと部分copy状態を検証。
- Windows target比較を共通path keyへ統一し、SQLite migrationをtransactional moduleへ分離。
- SQLite row、React model/conflict component、integration fixture supportを分離し、重複Desktop scan commandを撤去。

## ローカルで残っている実装作業

1. CLIへTOML naming設定、plan改訂、履歴整理を追加し、本rollbackの確認契約を確定する（T17）。
2. Core/Infraに加えてCLI/Desktop境界の自動試験を補強し、Windows固有試験とCI検証範囲を再確認する（T24）。

## 利用者の確認が必要な外部作業

1. bundleから得たWindows installerをWindows実機へインストールし、WebView2環境で起動・基本操作を受入確認する。

Windows runnerでの固有試験とbundle生成は確認済み。外部で残る工程はWindows実機でのinstaller受入確認のみ。ローカルの残作業とは区別して管理する。
