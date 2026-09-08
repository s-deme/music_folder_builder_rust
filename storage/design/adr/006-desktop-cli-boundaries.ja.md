# ADR-006: Desktop managed state と versioned CLI/release boundary を採用する

- 状態: Accepted
- 日付: 2026-08-26

## 背景

WebViewから任意DB pathをcommandへ渡せる設計はDesktopのtrust boundaryを広げる。非同期応答を画面のworkflow contextと結び付けないと、古いexecutionを新しいPlanからVerify/Rollbackできる。人向けstdoutだけのCLIと未固定release工程は自動化・配布物の追跡を難しくする。

## 決定

Desktop backendはapp-local DBとapplication service/job registryをprocess managed stateとして所有し、command引数からDB pathを除く。UIはreducerとgeneration/job tokenでworkflow contextを管理し、新Scan/Plan/改訂時に古いdownstream IDと応答を無効化する。Tauri capabilityとCSPはreleaseに必要なlocal asset/IPCだけを許可する。

手動targetはbackendの検証commandでPlan/item/generationへ束縛した短命・一回限りのopaque capabilityへ変換し、実際のPlan改訂commandはraw targetを受け取らない。capability consume後もCoreがpersisted root/rulesに対してtargetを再検証する。

CLIはDesktopと同じCore workflowを使い、TOML設定、Plan改訂、archive/cleanup、recovery、確認tokenを提供する。全commandはversioned JSON envelopeと安定exit codeを選択可能にする。releaseはtoolchain/dependencyを固定し、Linux/Windows gate、SBOM、checksum、provenance、署名input、installer smokeを自動化する。本番証明書とWindows実機受入は外部条件として分離する。

## 結果

rendererは保存領域の権限を持たず、stale operationをbackendでも拒否する。CLIの破壊的commandには非対話automationでも明示confirmationが必要になる。release artifactはcommitとmetadataを照合できる。
