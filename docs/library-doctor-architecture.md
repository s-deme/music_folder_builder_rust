# Library Doctor Phase 1 / Desktop

## 目的と責務

既存アプリに音楽ライブラリ診断を追加する。整理先、Plan、移動権限は不要。
音楽ファイルは読み取り専用で、SQLiteにはscan snapshot、メタデータキャッシュ、診断実行・Issueを保存する。
既存の `diagnostics` はログ書出し・保守の入口のまま。Desktopには独立した「診断」タブを追加する。

| 層 | 責務・再利用 |
| --- | --- |
| Core `doctor.rs` | DoctorUseCase、診断用モデル・Store port、純粋な比較・グループ判定。SQLite、Tauri、実ファイルI/Oへの依存なし |
| Core `usecases.rs` | 既存ScanUseCaseの並列列挙・bounded queue・バッチ保存・進捗・取消を再利用。開始済みscanを扱える入口を追加 |
| Infra `windows_fs.rs` | 同じ走査・SHA-256・native file identityを再利用。DoctorFileSystemだけAAC／OPUS／WAVも対象にし、外部画像は対象外 |
| Infra `lofty_reader.rs` | 既存Lofty読取にGenreと埋込画像有無を追加。既存の文字列を正規化せず保持 |
| Infra `sqlite/doctor.rs` | 独立した診断実行・Issue保存、scan snapshotと警告の読取、参照・絞り込み |
| CLI `doctor.rs` | 引数と共通JSON envelope、終了コード、進捗・Ctrl+Cを接続 |
| Desktop `doctor.rs` / UI `doctor.tsx` | 同じCore use caseで診断を開始・取消し、保存履歴、Issue・アルバムのページ表示、ジャケット閲覧を提供。整理側とは実行IDを分離し、走査・変更処理との同時実行を防ぐ |
| Infra `doctor_view.rs` / `artwork.rs` | 保存結果の表示用モデルとページング、閲覧時の画像読取・縮小・容量制限付きキャッシュ |

## データと互換性

- `TrackMetadata` に `genre: Option<String>`、`has_artwork: Option<bool>` を追加。
  `None`の画像有無は古い読取結果の「未調査」で、`Some(false)`の「なし」と区別する。
  旧JSONはserde defaultで読める。タグの読取失敗は `metadata=None`、読取成功した欠損は各フィールドのNone。
- ファイル情報は既存のimmutable `scan_items` に置く。パスは既存のnative byte/UTF-16保存を使用。
  表示JSONも既存lossless path envelopeを使う。
- 新規SQLite migration **17** は `doctor_runs`、`doctor_issues` と参照・検索indexを追加。
  既存Plan・operation journal・移動rollbackのテーブルや認可ハッシュ形式は変更しない。
  スキーマ17を扱えない旧バイナリは既存のtoo-newチェックで拒否する。逆マイグレーションは提供しない。
- `doctor_runs`: ID、scan ID、status、rule version、開始・終了時刻、概要JSON。
  概要にファイル数、cache hits、失敗観測件数、Issue数、致命的エラーを保存する。
- `doctor_issues`: run ID、ordinal、コード、severity、category、Issue JSON。
  JSONには対象file IDs、根拠、比較値、rule versionを含む。ファイル情報をIssueごとに複製しない。
  file IDはscan ID＋lossless pathのSHA-256先頭128bitから導出し、再読取しても同じ参照になる。
- 実行状態はRunning／Completed／Partial／Cancelled／Failed。
  読取や再検証の一部失敗はPartial、Ctrl+CはCancelled、列挙開始不能などはFailed。
  終了状態とIssueは同じtransactionで確定する。強制終了・電源断・DB書込不能ではRunningが残り得るが、成功とは扱わない。
  失敗・取消でもrun IDとscan IDから保存済み状態を参照できる。
- 既存scan認可snapshot v1は従来フィールドのまま維持する。Genre・画像有無は整理認可の入力にしない。
  診断はそのscanのimmutable行に基づく。診断結果は将来のファイル状態を保証しない。
- scan履歴を既存の明示的purgeで消した場合、その診断実行・Issueも外部キーCASCADEで削除される。
  キャッシュの追記専用制約、Planの不変性、移動操作ログの直列・順序付き記録は維持する。

## 走査、キャッシュ、安全性

既存走査は整理の認可用に全ファイルのSHA-256を取得している。これを診断でも再利用する。
重複判定は `(size, SHA-256)` のindexに集約し、総当たりや重複用の追加ハッシュ読取はしない。
同じサイズの候補だけをハッシュする最適化は、既存の強いキャッシュ検証を弱めるため導入していない。

キャッシュはlossless path、size、mtime、SHA-256、native identity、fingerprint version、reader ID/version、
metadata schema、設定、path normalization versionが一致した成功結果のみ再利用する。
今回reader versionを `lofty-v2`、metadata schemaを2に更新し、古い結果をヒットさせない。
読取失敗は成功キャッシュとして再利用しない。

タグ読取後にfingerprintを再検証し、変更があればそのタグを保存しない。この共有チェックは整理側にも適用される。
Doctorはscan終了後にもファイルを再検証し、変更・アクセス不能なら `file_changed` を記録して診断候補から除外する。
そのためcold scanでは通常最大3回、warm scanでは通常2回の全体ハッシュ読取が必要になる。
これは原子的な全ライブラリsnapshotではない。各ファイルの最後の観測以降に起きた変更や、
他プロセスが内容・時刻を往復させる競合まで保証しない。ファイルをロックして長時間占有しない。

reparse/symlinkは追跡しない。ルート祖先も確認し、列挙時はreparseディレクトリの下降を止める。
個々の列挙・fingerprint・タグ読取失敗は理由とパスを保存し、残りを続行する。
取消は走査・ファイル検証・ルールのグループ間で確認する。1ファイルの同期ハッシュ／Lofty読取中は即時中断しない。
`failures` はscan警告と再検証失敗の観測件数であり、異なるファイルの厳密な総数ではない。
読取失敗のIssueとscan警告には同じ原因が現れることがあるため、Issue数とfailure数も一致しない。

## 比較・アルバムの規則（rule version 1）

Artist/Albumの比較キーはNFKC→Unicode lowercase→空白の分割・単一スペース結合。
前後空白、大文字小文字、全半角、結合文字、連続空白、改行・タブを扱う。
除去する不可視文字はU+200B ZERO WIDTH SPACE、U+2060 WORD JOINER、U+FEFF BOMのみ。
ZWJ/ZWNJなど意味を変え得る文字は保持する。言語依存照合・完全なUnicode case folding・読みや別名の推測はしない。
同じキーに異なる元文字列が存在すると候補を出し、元文字列とキーを分けて保存する。
候補は同一人物・同一作品の断定でも、自動統合でもない。
Albumの表記比較は正規化したAlbum Artist、欠損時はArtistを文脈とする。どちらも不明なら比較しない。

アルバム整合性は「フォルダ＋正規化Album」で粗く分け、次の保守的規則を使う。

1. 直下の `CD1`、`Disc 2`、`Disk3` 形式（正の整数付き）のディスクフォルダは親へまとめる。
2. Artistが同じならAlbum Artist不一致も検出するためまとめる。
3. Artistが複数でも全員のAlbum Artistが同じ非空値ならコンピレーションとしてまとめる。
4. それ以外はAlbum Artist／Artistで分ける。不明なコンピレーションは分割され得る。
5. 別フォルダの版はまとめない。同じフォルダ・同じタグに別版を混在させた場合の判別はできない。

trackはディスクごとに扱い、Disc欠損・0は仮にdisc 1。Track欠損・0は番号比較から除外する。
1から最大観測番号までの欠番を**範囲**として提示し、巨大な番号でも巨大配列を作らない。
末尾の不足・実際の曲順・完全収録・ボーナストラック・意図した抜粋はローカル情報だけでは保証できない。
Genre等の属性不一致は非空の元値同士を比較する。欠損は別の欠損ルールで扱う。

## Issue一覧と重要度

Criticalは「診断の根拠を取得できなかった」を意味し、音声破損の断定ではない。
Warningは識別や曲順に影響し得る項目、Infoは一般的に意図した状態でもある項目。

| コード | 重要度 | 根拠 |
| --- | --- | --- |
| `read_failed` | Critical | メタデータ取得不可。欠損ルールを適用しない |
| `file_changed` | Critical | scan後のfingerprint不一致／アクセス失敗。判定から除外 |
| `scan_warning` | Critical | 読取・列挙・cache・reparse除外などのscan根拠。部分結果であることを示す |
| `missing_title`, `missing_artist`, `missing_album`, `missing_track` | Warning | 曲識別や順序に必要な情報なし |
| `missing_album_artist`, `missing_disc`, `missing_year`, `missing_artwork` | Info | 単一Artist・単一discなど、欠損が普通のライブラリもある |
| `artist_variant`, `album_variant` | Info | 同じ比較キーに異なる元文字列 |
| `exact_duplicate` | Info | 全体SHA-256とsize一致。native identityが不明な別名を含む場合あり |
| `same_file_paths` | Info | 全候補のnative identityが同じ。hard link等で同じ実体 |
| `track_gap`, `track_duplicate`, `disc_inconsistent` | Warning | ディスク内欠番候補・重複番号、Disc欠損混在／番号の飛び |
| `album_artist_inconsistent`, `year_inconsistent`, `genre_inconsistent`, `format_mixed` | Info | 同一グループ内の非空値不一致。破損とは断定しない |

完全重複結果にはパス・形式・size・mtime・hash・identityを表示する。残すファイルの選択、削除、移動はしない。
SHA-256は全ファイルのバイト比較の根拠で、デコード音声やタグを無視した同一曲判定は含まない。

## CLI

```powershell
music-folder doctor scan --source "D:\Music" --db "D:\Reports\music.db"
music-folder doctor show --run-id <ID> --db "D:\Reports\music.db"
music-folder doctor issues --run-id <ID> --severity warning --code track_gap --db "D:\Reports\music.db"
music-folder doctor duplicates --run-id <ID> --db "D:\Reports\music.db"
music-folder doctor albums --run-id <ID> --db "D:\Reports\music.db"
music-folder --output json --events jsonl doctor scan --source "D:\Music" --db "D:\Reports\music.db"
```

`doctor scan` は `--workers` も利用可能。`--severity` はcritical／warning／info、`--code`は上表のコード。
未定義コードはusage error。`issues`は全Issue、`duplicates`と`albums`は該当categoryのみ。
`show`は概要、一覧はIssueと参照されたファイル情報を分けて返す。Human表示は整形JSONを含む。
既存stdout一行JSON envelope 1.1、stderr JSONL進捗と終端イベントを利用し、run/scan IDを相関付ける。
正常診断は0、引数不正2、部分失敗4、致命的失敗5、取消9。Issue検出自体はエラー終了ではない。
保存結果を読むコマンドは参照成功なら0を返し、元実行の状態を結果内に残す。
CLIは全結果を読むため巨大な一覧の出力自体はO(n)。Desktopの一覧はページングする。

## Desktopの診断画面

「診断」と「整理」を分け、診断は整理先やPlanなしで実行する。保存履歴から結果を再表示し、
部分失敗・取消・未完了を区別する。Issueは重要度・コードで絞り込み、根拠と対象ファイルを表示する。
アルバム一覧は検索とページングに対応し、選択結果の切替後に古い応答を表示しない。

ジャケットは閲覧時に埋込画像、次にフォルダ画像を読み取り、最大256pxのPNGとして表示する。
入力は16MiB、画像寸法は4096px、デコード割当は64MiB、キャッシュは32MiBに制限し、同時読取は2件まで。
reparseやルート外の画像を拒否し、読取前後のfingerprintで変更を検出する。元画像は変更しない。
フォルダ画像があっても埋込画像の欠損Issueはそのまま残す。

「このフォルダを整理」は元フォルダの選択だけを整理画面へ渡す。Planや移動権限は診断結果から引き継がず、
既存のscan → plan → applyの確認を使う。

## 形式とテスト方針

Lofty 0.22.4の既存実装を利用。拡張子だけで対応済みとは扱わない。

| 形式 | 検証範囲 |
| --- | --- |
| MP3／FLAC／M4A／OGG Vorbis | 既存の日本語タグfixtureで読取検証。全codec・全タグ方言の保証ではない |
| MP3 | 追加でGenre・埋込画像有無、破損入力、cache、完全重複、入力不変を検証 |
| WAV | synthetic PCM RIFFの読取とタグ欠損・画像なしを検証。全WAVタグ方言は未検証 |
| AAC／OPUS | Loftyに読取実装あり、Doctorの走査・壊れた入力の継続は検証。正常音声・タグfixtureは未検証 |

Coreテストは正規化・元値保持・アルバム文脈・複数ディスク・欠番・属性・同サイズ別hash・identity・取消を検証。
Infraテストは実fixture＋一時DBで読取・埋込画像・cache hits・旧reader無効化・v16→v17・部分失敗・
取消・タグ読取中の変更・入力bytes/mtime不変を確認する。テスト用の変更は一時コピーのみ。
CLIプロセステストはJSON一行契約、進捗終端、終了コード、参照、severity/code絞り込みを確認する。
既存のPlan認可・apply/verify/rollback回帰はworkspace testに含む。

走査queueは既存のbounded構造、診断はO(n)のメタデータ・参照を保持しBTree indexで概ねO(n log n)。
画像バイト列はLofty読取中のworker分だけで、全曲分を診断メモリ／DBに残さない。
1万曲syntheticタグの計測用テストは `cargo test -p music-folder-core --test doctor ten_thousand_tracks -- --ignored --nocapture`。
実音楽1万曲のdisk I/O・ピークメモリ・NAS・大画像・全Windows長パス構成の性能は保証しない。

2026-10-05の検証実績:

- READMEのDocker `make validate` 等価コマンド: format、Clippy（警告をエラー扱い）、
  Rust **184 passed／0 failed／2 ignored**、UI **18 passed／0 failed**。
  TypeScript型検査・テスト用／本番ビルド・CSP検査を含む。npm auditの脆弱性検出は0件。
  この環境ではDockerのSQLite回帰・プロセス応答が非常に遅く、テスト所要時間を製品性能の指標にしない。
- Windows `cargo test --workspace --locked --offline`: 191 passed、0 failed、2 ignored。
  ignoredは手動性能試験。Doctorの1万件試験は別途明示実行した。
- 最終変更後のCore診断6件・Infra診断10件、Windowsパス3件も再確認。
  symlink作成権限のないWindowsではjunctionへ切り替えるテストを追加し、reparse除外をスキップせず確認した。
  当該Windows専用テストのClippyも警告なし。
- CLI公開スキーマ1.1の自己テストと、実プロセスのdoctor scan JSONのschema検証に合格。
- Doctorルール性能試験: Windows、Rust 1.97.1、debug build、10,000曲／1,000フォルダ、
  1アルバム10曲、完全な合成タグ、画像本体・disk I/Oなし、Issue 0件。
  1回の測定で **1.2312652秒**。Docker回帰検証と並行実行しており、専用の性能測定環境ではない。
  実ライブラリの走査時間・ピークメモリへの外挿はしない。

## 後続フェーズ

詳細音声品質、ファイル名とタグの不一致、画像破損・サイズ・一致判定の診断ルール、
Metadata Duplicate／類似曲、Health Score、タグ修正・一括修正、MusicBrainz／Discogs／AcoustID連携は未実装。
自動削除・自動リネーム・新しい整理機能も今回の範囲外。
タグ修正Undoは移動rollbackと別設計が必要。変更前後のタグ値だけで完全復元できると仮定せず、
バックアップ、原子的書込、変更後の競合、既存移動履歴との整合性を設計してから着手する。
