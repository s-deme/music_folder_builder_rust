# 並列 Scan パイプライン設計

## Data flow と上限

```text
lossless enumerator --bounded N1--> stat/identity/hash workers --bounded N2--> metadata workers
        |                         |                                  |
        +------ diagnostics ------+---------- cache decisions -------+
                                           --bounded N3--> single SQLite writer
                                                               |
                                                         progress/event sink
```

enumeratorはiteratorで一件ずつ読み、pathをWindows UTF-16 code unit列のまま `LosslessWindowsPath` にする。表示文字列は同時に生成してよいが、列挙、重複、cache key、snapshotでは使わない。directoryはhandle相対で開き、reparse pointを既定で追跡しない。reparse point、access denied、非対象拡張子も順序付きdiagnosticとしてwriterへ送る。

音楽拡張子（FLAC/MP3/M4A/OGG）はhash後にmetadata workerへ、同梱asset拡張子（jpg/jpeg/png/webp/gif/bmp）はmetadata読取なしでwriterへ送る。各file recordはordinal、lossless path、asset kind、file identity、size、mtime、content hash、metadata resultまたはtyped errorを持つ。writerだけがcatalog/cache/resultとscan snapshotをbatch書込みする。

初期値はmetadata worker `W = min(8, max(2, logical_cpu_count))`、hash workerはstorage profileに応じ2〜4、N1/N2/N3は各worker数の4倍、writer batchは250件とする。HDD/network profileはhash/metadata concurrencyを2へ下げる。memory上の未処理件数は各bounded queue、worker数、batch/pageの和に制限し、file件数に比例させない。

## Immutable snapshot

`scan_runs` は `running` の間だけitemを追加でき、writerが全producer終了、件数、diagnostic、cache resultをcommitした後に `completed` へcompare-and-swapする。Plan repositoryはcompleted runだけを受け付ける。completed後の `scan_items` update/delete APIは提供しない。cancel/error時のpartial itemは診断には残せるがPlan入力にしない。

`scan_items` は `library_files` の外部キーだけではなく、その時点の次をcopyして保存する。

- lossless source path、WindowsPathKey version/key
- volume serial + 128-bit file ID（取得可能な場合）とidentity kind/version
- size、mtime、link/reparse observation
- SHA-256 algorithm/version/digest
- asset kind、disposition、warning/error code
- immutable metadata result ID、reader ID/version、metadata schema version

`library_files` は最新catalog/cache hintであり、過去scanのPlanはそこからpath/stat/metadataを読み直さない。後続scanが同じfileを発見しても新しいscan itemを作り、古いsnapshotを変更しない。

## Source identity とhash

workerはno-follow handleからWindows `FILE_ID_INFO` 相当を取得し、volume identityとfile IDを組にする。identity未提供のfilesystemではその事実をcode化する。全move候補はPlan完成までにSHA-256が必須であり、scan時に計算するのを既定とする。hash中は同じhandleを使い、前後のidentity/size/mtimeが変化したrecordを `source_changed_during_scan` としてPlan不能にする。

warm scanでhashをreuseできるのはstable file identity、size、mtime、hash algorithm/versionがすべて一致し、前回resultがcompleteな場合だけである。identityを取得できない場合、mtime/sizeだけでstrong hashをreuseしない。Plan preflightは現在sourceを再hashし、applyはstaging copyと同時に再hashするため、scan hash cacheだけを削除認可にしない。

## Metadata cache とreader version

metadata cache keyは次の完全一致とする。

```text
source_fingerprint
+ reader_id
+ reader_version（crate/app buildだけでなくtag選択挙動を変える版）
+ metadata_schema_version
+ reader_config_hash（album artist優先規則等）
```

cache resultはappend-onlyで、hitはそのimmutable result IDをscan itemへ保存する。reader/version/schema/configが変われば必ずmissとしてLoftyを再実行する。成功、タグ不足、unsupported、破損、I/O失敗を別status/codeにし、一時I/O errorは設定したTTL後に再試行できる。`reader_version` を単なるpackage versionから推測せず、adapterが明示する定数としてfixture testで固定する。

## Cancellation、progress、diagnostic retention

cancel時はenumeratorを先に停止し、workerは既に受け取ったrecordをwriterへ送るか安全に破棄してjoinする。writerは受信済みbatchをcommitし、runを `cancelled` にしてproducer別件数を保存する。channel切断、panic、DB errorでもrun lifecycle guardが `failed` を確定し、runningのまま残さない。ただしprocess crashで残ったrunning runはstartup recovery診断で `abandoned` とし、Plan入力にしない。

progressは100ms程度にthrottleし、job ID、scan run ID、単調増加event sequence、phase、列挙/identity/hash/metadata/cache/write件数、bytes、elapsedを送る。UI eventは取りこぼし可能な観測値であり、権威状態はDB query/job snapshotから再取得する。progress/debug eventは既定7日、通常scan diagnosticは30日、failed/abandonedは180日を下限にretentionし、scan item/metadata provenanceはworkflow archiveまで保持する。

## Bounded Plan handoff

Plan builderはcompleted scanを `(scan_run_id, ordinal)` cursorで固定pageずつ読み、Coreでtarget draftを生成してbuild ID付きSQLite stagingへbatch保存する。全itemをmemoryへ集めない。duplicate/conflict、disc anchor、画像candidateはstaging indexと安定ordinal順の複数passで解決する。revisionもparent Planをcursor読取りして同じstaging pipelineへ流し、単一行UPDATEを行わない。

## Performance と試験

性能目標（Windows NVMe、100,000件、cache warm）はRSSがfile件数に比例しないこと、metadata warm scanがcold scanよりタグ読取件数を95%以上削減すること、phase別duration/bytes/cache理由を毎回保存することである。受入benchmarkは同一fixture/設定でcold/warmを各3回実行しmedian、件/秒、RSS peak、hash/tag読取件数、DB commit時間を比較する。絶対秒数はstorage/fixtureを併記し、実装前にCI閾値を固定しない。

試験はbounded channelのbackpressure、cancel各phase、writer failure、reader version bumpによるcache miss、過去snapshot不変性、同一pathのfile差替え、hash中変更、identityなしfilesystem、unpaired surrogate path、reparse cycle、100,000件でのRSS上限を含む。Windows固有identity/path/reparse試験はWindows CI runnerで実行する。
