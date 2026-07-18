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

Desktop の状態 DB は Tauri の `app_local_data_dir` 配下の `music-folder.db` とする。Desktop backend が起動後の初回参照時に親 directory を作成して path を UI に通知し、全 Desktop command はその path を共有する。UI は DB path を設定項目として表示しない。CLI は automation・検証用途の明示的な `--db` を維持する。

## 実行状態

`scan_run(completed) -> plan_run(completed) -> execution_run(dry_run|apply) -> verify_run -> rollback_run -> verify_run`。apply は `plan_item` の snapshot/hash を検証してから実行し、対象 plan の内容変更を拒否する。apply/rollback は一 worker の順序付き transaction/log flush で処理する。将来の並列 apply は directory lock と独立 target 集合を導入した scheduler に限定する。

apply の実装順序は `plan item検証 -> target存在確認 -> 同一volume rename | 異volume copy -> size検証 -> source delete -> operation log commit` とする。dry-runは同じ事前条件評価とlog保存を行うが、filesystem mutationを呼ばない。

## 命名・asset・plan revision

Plan は naming rules snapshot（artist/album/disc/filename/duplicate suffix、元音楽・画像ファイル名の保持設定）を保存する。テンプレート展開は Core の純粋関数とし、数値書式と `[{field}]` 形式の条件ブロックを解釈してから component sanitization を行う。音楽 target の重複は suffix template を item 固有値で展開し、なお重複する場合は安定した連番を追加する。それでも既存 target または path risk があれば skip する。

suffix適用後も残る同一Plan内の衝突は、理由codeだけでなくPlan snapshot内の衝突groupとして保存する。groupは安定したID、比較に用いた正規化target path、および同じPlan内の全member item IDを持つ。各memberのsource pathはPlan itemから取得する。Plan item pageはgroup ID・相手件数だけを返し、展開時のdetail queryがgroup全memberのitem ID/source pathと共通targetを返す。これにより大量一覧を肥大化させず、各衝突行から相手を直接確認できる。plan revisionは全itemの衝突groupを再評価し、親Planのgroupを流用しない。既存targetとの衝突はapply/dry-runのitem結果でsourceと既存target pathを併記する。

CoreはUIから独立した命名規則validation/preview APIを持つ。validationはtoken構文、field allow-list、必須component、生成後path policyを返す。NamingRulesの追加fieldはserde defaultを持ち、保存済みsnapshotを読み取れる後方互換性を維持する。

metadataが一部不足する音楽itemは、album artist/artistのfallback値を `UnknownArtist`、albumのfallback値を `Unknown_Album` として通常の命名templateを展開する。読み取れた値はfallbackで置き換えない。metadata全体を読み取れない場合は同じartist/album fallbackを使い、title等に依存する命名を避けて元ファイル名を保持する。itemは移動可能な場合も `risk=metadata_missing` と具体的な不足理由を保持する。生成targetは通常の重複解決、既存target確認、source同一判定、path policyを通し、Plan snapshot確定後に再計算しない。

path policy はtarget path全体を240文字まで許可する。component単体の文字数上限は設けず、ファイル名やフォルダ名が80文字を超えてもpath全体が240文字以下なら許可する。文字数はRustの `chars()` によるUnicode scalar value数で数え、上限値そのものは許可し、上限超過時だけ拒否する。Coreの長さ診断は実測文字数・上限文字数を保持し、Plan reason、命名preview、CLI/Desktop adapterが「パス全体が長すぎます: 241文字（上限240文字）」の日本語表示へ変換する。内部codeは利用者向け理由へ露出しない。既存のrisk分類 `path_too_long` は集計・filterとの後方互換性のため維持する。

Plan reasonと命名validation issueは永続化・判定用の安定した内部codeを維持し、表示adapterで全codeを日本語へ変換する。既知codeは具体的な日本語文言とし、未知codeは「詳細不明の理由があります」のような日本語fallbackに、調査・copy用の補助情報を分離して提示する。内部codeそのものを主たる利用者向け理由として表示しない。

scan は音楽と画像 asset を区別して snapshot に保存する。Plan は音楽 item が決定した source-directory-to-target-directory 対応を根拠に jpg/jpeg/png/webp/gif/bmp を対応付ける。画像は対応音楽がない・複数 target に曖昧に対応する場合に skip とし、source image filename を保持する設定では同一 directory 内で `_2` 以降の連番を付ける。

画像の対応先が複数ある場合は、画像itemに `image_destination` conflict groupを割り当て、候補target directoryごとに根拠となる音楽Plan item IDを保存する。Plan pageは候補数を返し、既存のconflict detail APIは種類に応じて候補directoryと音楽itemのordinal/source pathを返す。候補選択は画像ファイル名をcandidate directoryへ結合して既存のPlan revisionへ渡し、親Planを変更しない。

手動 target 指定は completed plan の item を更新しない。Core の `RevisePlanUseCase` が親 plan と変更集合を読み、新 plan と全 item snapshot/hash を生成する。apply は従来どおり新 plan の保存済み item のみを入力とする。

## 互換性・非対象

Python 版と、段階遷移、既定 no-overwrite、reparse point 非追跡、path sanitization、命名テンプレート、同梱画像の移動、異ボリューム copy-verify-delete、操作ログ、逆順 rollback を互換基準とする。DB は新 schema とし既存 SQLite の直接読取り/移行は初期版対象外。タグの完全な byte-level 同一性、画像/歌詞編集、ネットワークストレージ固有最適化、並列 apply/rollback も対象外。
