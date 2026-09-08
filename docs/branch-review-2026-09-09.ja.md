# ブランチ整理の判断（2026-09-09）

対象はDependabotの14本。前回の検証済みCI短縮を先に保存し、必要な9本をmainへ通常のmerge commitで統合する。残り5本は現用途で移行が不要なため採用せず、PRをcloseしてbranchを削除する。統合したbranchもmainのremote反映後に削除する。

集約設定の反映直後に追加生成された#16〜18も精査した。#16を追加統合、#17・18を不採用とし、合計10件を統合、7件を不採用とする。

| PR | 更新 | 判断・理由 |
| --- | --- | --- |
| #2 | upload-artifact 7.0.1 | 統合。既存PRはWindowsのbundle／installer試験まで成功。archiveの既定動作を維持する。 |
| #3 | cache 6.1.0 | 統合。read-only cache対応を含む。downloadだけをcacheする既存方針は維持する。 |
| #4 | setup-python 7.0.0 | 統合。manifest取得の検証・retry修正を含む。削除されたpip-install入力は本projectで未使用。 |
| #5 | download-artifact 8.0.1 | 統合。hash不一致を既定で失敗にする更新は配布物検証の方針に合う。 |
| #6 | clap 4.6.6 | 統合。同一系列の修正更新。CLI実行・JSON・確認tokenの契約を全体テストで確認する。 |
| #7 | react-dom／型定義 19 | 不採用。React 18と単独では整合しない。React 19の移行が必要になった時に一括検証する。 |
| #8 | tauri-plugin-dialog 2.7.3 | 統合。同一系列の修正更新。関連するfs 2.5.2も含む。 |
| #9 | serde_json 1.0.151 | 統合。同一系列の修正更新。永続化・CLI JSON契約を全体テストで確認する。 |
| #10 | plugin-react 6.1.1 | 統合。同一major内の更新。既存React 18でUIテストを維持する。 |
| #11 | sha2 0.11 | 不採用。現行SHA-256用途でAPI移行は不要。依存先が0.10を使うため暗号依存の二重化も増える。 |
| #12 | TOML 1.1 | 不採用。CLI設定の構文・parser系列を切り替える必要は現時点でない。 |
| #13 | Vite 8.2.2 | 統合。同一major内の更新。test用／production buildとCSPを確認する。 |
| #14 | TypeScript 7 | 不採用。compilerのmajor移行は今回の保守範囲に不要。型検査の移行が必要になった時に行う。 |
| #15 | React／型定義 19 | 不採用。react-dom 18と単独では整合しない。#7と同時に移行すべき更新。 |
| #16 | checkout 7.0.1／setup-node 7.0.0 | 統合。fork checkoutの安全対策、cache権限処理、不要な認証token環境変数の削除を含む。既存の入力設定は互換。 |
| #17 | UI group（5更新） | 不採用。#7・14・15で見送ったReact 19・TypeScript 7の再提案。 |
| #18 | Rust group（9更新） | 不採用。sha2／TOMLの再提案に加え、lofty 0.25・rusqlite 0.40へのAPI系列変更も含む。音声タグ・DBの移行が必要な時に分けて検証する。 |

古いPRのWindows失敗には、mainで修正済みのDesktop ACLテスト失敗が含まれていた。過去の失敗表示だけで修正更新を棄却せず、統合後のmainを検証する。

Actionsの変更点は各公式release noteを確認した：
[cache](https://github.com/actions/cache/releases/tag/v6.1.0)、
[setup-python](https://github.com/actions/setup-python/releases/tag/v7.0.0)、
[upload-artifact](https://github.com/actions/upload-artifact/releases/tag/v7.0.0)、
[download-artifact](https://github.com/actions/download-artifact/releases/tag/v8.0.0)。

追加分は[checkout](https://github.com/actions/checkout/releases/tag/v7.0.0)、[setup-node](https://github.com/actions/setup-node/releases/tag/v7.0.0)の公式release noteを確認した。追加統合はworkflowのAction参照だけで、Docker全体検証済みのRust／UIコード・lockfileを変更しない。

今後は通常作業をmainで行う。Dependabotは月次のversion更新をecosystemごとに1つへgroup化し、各1件の上限にする。security更新は無効化しない。
