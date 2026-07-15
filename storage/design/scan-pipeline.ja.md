# 並列 Scan パイプライン設計

```text
File enumerator --bounded N1--> tag workers (max W) --bounded N2--> single SQLite writer
       |                                  |                            |
       +-------------- progress ----------+------------> Tauri event sink
```

enumerator は `walkdir` 相当の iterator で一件ずつ送り、reparse point/非対象拡張子を item として記録する。音楽拡張子（FLAC/MP3/M4A/OGG）は tag worker へ、同梱 asset 拡張子（jpg/jpeg/png/webp/gif/bmp）は metadata 読取なしで writer へ送る。tag workers はファイル stat と metadata cache を照合し、hit はタグ読取を省略、miss のみ lofty を呼ぶ。writer だけが scan item、asset snapshot、metadata cache、metric をバッチ書込みする。これによりメモリ上の未処理件数は `N1 + W + N2 + batch` に上限化される。

初期値は `W = min(8, max(2, logical_cpu_count))`、N1/N2 は各 `W * 4`。HDD/ネットワークでは設定 profile が W を 2 に下げられる。progress は throttling（例: 100ms）し、列挙/読取/書込みの件数・bytes・elapsed を送る。キャンセル時は producer を停止し、既に writer に届いた batch を commit、run を `cancelled` とする。

性能目標（Windows NVMe、100,000 件、cache warm）は、RSS がファイル件数に比例しないこと、warm scan が cold scan よりタグ読取件数を 95% 以上削減すること、phase 別 duration を毎回保存すること。受入ベンチマークは同一 fixture/設定で cold と warm を各3回実行し median、件/秒、RSS peak、DB commit 時間を比較する。絶対秒数は対象ストレージと fixture を併記して基準化し、実装前に CI 性能閾値を固定しない。
