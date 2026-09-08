# 要件トレーサビリティ

要件、主要設計、実装タスク、代表的な自動検証の対応を示す。タスクの完了状態は `storage/tasks/implementation-plan.ja.md` を正とし、未完了タスクを含む行はその要件全体の完了を意味しない。

| 要件 | 主要設計 | タスク | 代表的な検証 |
|---|---|---|---|
| REQ-SAF-001〜002 | `architecture.ja.md` workflow/Plan authorization、ADR-005 | T02、T06〜T08、T37、T44、T45 | workflow transition unit test、snapshot tamper integration test |
| REQ-SAF-003 | `architecture.ja.md` common preflight、ADR-003、ADR-005 | T07、T44、T51 | dry-run/Apply parity matrix、filesystem no-mutation test |
| REQ-SAF-004 | `architecture.ja.md` SafeTargetPath/no-replace、ADR-002、ADR-003 | T07、T38、T41、T44、T51 | target race、root confinement、existing-target fault test |
| REQ-SAF-005 | `architecture.ja.md` execution state/recovery、`sqlite-schema.ja.md` journal、ADR-003 | T36、T42〜T44、T46、T51 | crash-point recovery、orphan run終端、migration test |
| REQ-SAF-006 | `architecture.ja.md` execution disposition | T39、T44 | eligibility/risk matrix、metadata panic regression test |
| REQ-SAF-007 | `architecture.ja.md` write-ahead protocol、`sqlite-schema.ja.md` journal、ADR-003 | T41、T42、T46、T51 | mutation各段階のprocess-kill/resume/rollback test |
| REQ-SAF-008 | `architecture.ja.md` mutation lease、`sqlite-schema.ja.md` lease、ADR-003 | T43、T51 | CLI/Desktop 2-process競合、heartbeat/fencing test |
| REQ-SCN-001〜003 | `scan-pipeline.ja.md` bounded scan/versioned cache、ADR-004 | T04、T05、T11、T40 | scan/cache/cancel integration test、cache version migration、benchmark |
| REQ-SCN-004 | `scan-pipeline.ja.md` immutable snapshot/source identity、`sqlite-schema.ja.md`、ADR-004 | T40 | Scan A/B分離、source差替え、同size改変test |
| REQ-PLN-001 | `architecture.ja.md` Windows path/SafeTargetPath、`ui.ja.md` long path、ADR-002 | T02、T06、T13、T27、T32、T38 | path traversal/UNC/device/Unicode table test、manifest・long path Windows test |
| REQ-PLN-002〜003 | `architecture.ja.md` naming/collision、`sqlite-schema.ja.md`、`ui.ja.md` conflict detail | T13、T20〜T22、T29、T33、T39、T45 | naming/衝突golden test、SQLite adapter contract、UI behavior test |
| REQ-PLN-004 | `architecture.ja.md` Plan revision、ADR-002、ADR-004 | T14、T16、T18、T37、T38、T45 | create/revise parity、manual target confinement、snapshot integration test |
| REQ-PLN-005 | `architecture.ja.md` bounded Plan staging、`sqlite-schema.ja.md`、ADR-004 | T47、T52 | 10万item RSS/時間benchmark、page境界決定性test |
| REQ-MDA-001〜002、REQ-AST-001〜002 | `scan-pipeline.ja.md` metadata、`architecture.ja.md` asset/eligibility、`ui.ja.md` image conflict | T15、T16、T28、T31、T34、T35、T39 | metadata fixture/matrix、画像候補workflow、panic regression test |
| REQ-APL-001 | `architecture.ja.md` no-replace apply、ADR-003 | T07、T41、T42、T51 | same/cross-volume race、partial copy、disk-full test |
| REQ-APL-002 | `architecture.ja.md` preflight/source identity、ADR-004、ADR-005 | T40、T44 | source replacement/content mismatch test |
| REQ-APL-003 | `architecture.ja.md` reparse policy、ADR-002、ADR-003 | T38、T41、T44、T51 | Apply/Rollback直前reparse差替えWindows test |
| REQ-VRF-001、REQ-RBK-001 | `architecture.ja.md` VerifySubject/rollback、`sqlite-schema.ja.md` attempt log、ADR-005 | T08、T42、T44、T46、T51 | Apply後/rollback後verify、複数attempt、reverse rollback fault test |
| REQ-ERR-001 | `architecture.ja.md` typed boundary/diagnostic | T36、T39、T44、T49、T52 | typed error unit test、CLI JSON/exit code golden、redaction snapshot |
| REQ-OBS-001 | `sqlite-schema.ja.md` run/metrics、`architecture.ja.md` Desktop managed state | T03、T05、T09、T30、T46 | repository/metrics integration、Desktop startup、benchmark |
| REQ-OBS-002 | `sqlite-schema.ja.md` attempt ownership、ADR-005 | T46 | verify/rollback複数attemptと履歴query/migration test |
| REQ-OBS-003 | `sqlite-schema.ja.md` diagnostic retention、`architecture.ja.md` export/redaction、ADR-005 | T46、T52 | retention/load、cleanup保護、diagnostic export/redaction test |
| REQ-UI-001 | `ui.ja.md` virtual list/bounded UI | T10、T23、T25、T26、T47、T50 | cursor repository、stale page、UI behavior/performance test |
| REQ-UI-002 | `ui.ja.md` workflow、ADR-005、ADR-006 | T10、T18、T44、T48、T50 | workflow reducer、stale job、confirmation behavior test |
| REQ-UI-003〜004 | `ui.ja.md` logs/history、`sqlite-schema.ja.md`、ADR-005 | T14、T18、T19、T46、T48、T52 | history/attempt repository、retention、UI behavior test |
| REQ-UI-005 | `ui.ja.md` rollback workflow、ADR-005、ADR-006 | T18、T44、T48、T50 | rollback confirmation、stale execution、keyboard test |
| REQ-UI-006 | `ui.ja.md` workflow context ownership、ADR-006 | T48、T50 | new Scan/Plan/revisionで旧execution無効化、async stale response test |
| REQ-UI-007 | `ui.ja.md` accessibility | T50 | keyboard/focus/aria component test |
| REQ-CLI-001 | `architecture.ja.md` CLI boundary、ADR-006 | T17、T49 | CLI process JSON schema/exit code/config/confirmation golden test |
| REQ-SEC-001 | `architecture.ja.md` trust boundary、`ui.ja.md` CSP、ADR-002、ADR-006 | T30、T38、T48 | command argument tamper、CSP inspection、path confinement test |
| REQ-REL-001 | `architecture.ja.md` release integrity、ADR-006 | T32、T53、T54 | Windows artifact/manifest inspection、SBOM/checksum/provenance、test-signature verification |

ADR番号は `storage/design/adr/002-lossless-safe-target-path.ja.md` から `006-desktop-cli-boundaries.ja.md` を指す。

## 実装完了範囲とrelease gate

- T17、T24、T32、T33、T37〜T54は完了。traceability evidenceはCore/Infra/CLI/Desktop/UI、Windows path、release workflowの実行profileとmarkerを機械検証する。
- 外部受入: EA01のWindows実機確認とEA02の組織署名資格情報による本番署名。自動検証・実装の完了とは別に判定する。

`make validate` はDocker内のformat、Clippy、workspace test、UI typecheck、UI production build、依存関係auditをまとめて実行する。Windows固有試験とbundle生成はGitHub ActionsのWindows runnerで検証し、実機・本番署名の外部受入とは区別する。
