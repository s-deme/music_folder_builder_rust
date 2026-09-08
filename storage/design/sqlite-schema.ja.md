# SQLite スキーマ設計

## Connection、migration、version

起動時に全connectionで `PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;` を設定し、writer初期化時に `PRAGMA journal_mode=WAL;` を確認する。writerは一つのrepository serviceに集約し、scan/Plan stagingは250件または短時間間隔（設定値）でcommitする。queryは専用read connectionを使う。

`database_metadata` はsingleton行として `database_uuid`、`schema_version`、`created_at` を持つ。`database_uuid` はarchive/provenanceでDBを識別する値であり、filesystem mutationのprocess間排他はDBから独立したcanonical root scopeを使う。`schema_migrations(version INTEGER PRIMARY KEY, name, checksum, applied_at, app_version)` の各行とmigration sourceのchecksumが一致しなければwriteを拒否する。SQLite `user_version` は早期判定用に同じschema versionへ合わせるが、権威はmigration tableとする。

各migrationはmaintenance leaseを獲得し、active filesystem mutation、running run、非終端operation journalがないことを確認して `BEGIN IMMEDIATE` 内で適用する。table rebuildなど破壊的migrationは同directoryへ `create_new -> backup API -> integrity_check -> flush -> atomic no-replace publish` したbackupを先に作る。対応最大versionより新しいDB、checksum不一致、support window外の旧versionは自動変更しない。空DB、各対応旧fixture、途中失敗後の再実行、newer-than-app拒否をmigration testにする。

## Path とversion付き値の格納規約

権威となるWindows pathは `{*_path_raw BLOB, *_path_encoding_version INTEGER}` にUTF-16LE code unit列として保存し、even lengthとversionをCHECKする。`*_display_path TEXT` は表示/全文検索用の派生値、`*_path_key BLOB` はversion付き `WindowsPathKey` である。display値をsnapshot、衝突判定、filesystem操作に使わない。lossless pathを含むunique/indexはrawではなく意図したWindows比較を表す `(path_key_version, path_key)` に張る。

日時はUTC Unix millisecond、size/sequence/versionはINTEGER、hashはalgorithm列とBLOB、typed enumは安定したsnake_case codeで保存する。JSONはversion列を併記した設定snapshotまたは診断payloadに限定し、外部キーや実行判定をJSON内だけに置かない。

## Tables

### Scan とcache

| Table | 主な列 | 不変条件・用途 |
|---|---|---|
| `scan_runs` | id, source_root_raw/display/key, source_root_identity, snapshot_version, config_json/version, metadata_reader_id/version, metadata_schema_version, status, started_at, finished_at | `completed` のみPlan入力。完成後はimmutable |
| `library_files` | id, canonical_path_raw/display/key, last_size, last_mtime_ns, last_file_identity, last_seen_scan_id | 現在/既知catalog。UI検索とcache hint用で、Planのsnapshot根拠にはしない |
| `metadata_results` | id, source_fingerprint, reader_id, reader_version, metadata_schema_version, reader_config_hash, tag_json, status, error_code, created_at | append-only metadata cache result |
| `content_hash_results` | id, file_identity, size_bytes, mtime_ns, algorithm, algorithm_version, digest, created_at | 安全にreuseできる場合のhash cache。更新せず新規行 |
| `scan_items` | scan_run_id, ordinal, source_path_raw/display/key, file_identity_kind/value, size_bytes, mtime_ns, hash_algorithm/version/digest, asset_kind, disposition, metadata_result_id, warning_code | self-contained immutable snapshot。`library_files`の現在値を後からjoinしない |
| `scan_diagnostics` | scan_run_id, sequence_no, path_raw/display, severity, code, payload_json/version | 列挙不能、reparse point、cache miss理由等。attempt内で順序付き |

`metadata_results` のreuse unique keyは `(source_fingerprint, reader_id, reader_version, metadata_schema_version, reader_config_hash)` とする。error resultには期限を設けられるが、別reader versionで再利用しない。`scan_items` はmetadata値をimmutable result IDで参照するため、後続scanがcatalog/cacheを追加しても過去snapshotは変化しない。主要indexは `scan_items(scan_run_id, ordinal)`、`scan_items(scan_run_id, source_path_key)`、`library_files(path_key_version, canonical_path_key)` uniqueとする。

SQLite schema v16では `scan_items` の表示用pathを主キーから外し、`(scan_id, ordinal)` を主キー、`(scan_id, path_key_version, path_key)` とlossless native path列を一意キーにする。旧rowのordinalはlossless path順、path keyはversion付きCore `WindowsPathKey`でtransaction内backfillする。completed scanのupdate禁止triggerとrunning scanだけへのinsert制約はtable再構築後も維持する。

### Plan build、revision、conflict

| Table | 主な列 | 不変条件・用途 |
|---|---|---|
| `plan_runs` | id, scan_run_id, parent_plan_id, build_id, target_root_raw/display/key, target_root_identity, rules_json/version, path_policy_version, snapshot_encoding_version, snapshot_hash, status, started_at, finished_at | `completed` 後immutable。`parent_plan_id`はCore revisionだけが設定 |
| `plan_builds` | id, owner_instance_id, scan_run_id/parent_plan_id, expected_parent_hash, cursor, status, started_at, heartbeat_at | bounded stagingの所有権とresume/cleanup判断 |
| `plan_stage_items` | build_id, ordinal, source snapshot列, draft relative target raw/key, duplicate key, anchor data, decision, issue summary | build中だけ存在。`(build_id, target_key)`をindex化 |
| `plan_items` | id, plan_run_id, ordinal, decision, source path/identity/stat/hash列, target path raw/display/key, target_root_id, relative_components_blob, path_policy_version, target_origin | applyの唯一の入力。完成行を更新しない |
| `plan_item_issues` | plan_item_id, sequence_no, severity, code, details_json/version | AdvisoryとBlockingを分離。`risk != none`を実行禁止条件にしない |
| `plan_conflict_groups` / `plan_conflict_members` | id, plan_run_id, kind, target_path_key / conflict_group_id, plan_item_id | 同一Plan内の全衝突member |
| `plan_conflict_candidates` / `plan_conflict_candidate_members` | conflict_group_id, ordinal, target directory path列 / conflict_group_id, candidate_ordinal, plan_item_id | 画像destination候補と根拠音楽item |
| `plan_revision_changes` | plan_run_id, sequence_no, parent_plan_item_id, change_kind, requested_target_raw/display, reason_code | ChangeSetの監査。変更後targetはplan_items側でSafeTargetPath化 |

Plan buildはstagingへbatch insertし、indexed queryでduplicate/conflictを解決する。finalize transactionは `plan_runs`、全 `plan_items/issues/conflicts/revision_changes`、snapshot hashを一括確定し、最後にstatusを`completed`へする。失敗/cancel時はcompleted Planを公開しない。期限切れ `plan_builds` はowner/heartbeatを確認してmaintenanceが削除する。

schema v16のPlan graph triggerはitem/group/member/candidateの参照先Planを一致させる。paged build中のcandidate memberは後続itemへのforward referenceを許すが、Planを`completed`へ遷移する同一transactionで欠落item/candidateとcross-Plan参照を拒否する。completed Planのrowを別Planからreparentできないよう、update guardはOLD側とNEW側の両Plan statusを検査する。

`plan_runs.snapshot_hash` は `plan_items` をordinal順に読み、各fieldを `field_tag + version + byte_length + raw_bytes` でcanonical encodingする。対象はordinal、lossless source/target path、decision、issue severity/code、source expectation、target root ID、path policy versionであり、display path、翻訳文、画像候補や衝突detailは含めない。NULLと空値を別表現にする。Plan確定時とpreflight時は同じCore encoderを使用し、adapter独自のSQL文字列連結で再実装しない。

### Attempt、preflight、journal、verification

| Table | 主な列 | 不変条件・用途 |
|---|---|---|
| `workflow_attempts` | id, root_scan_id, kind, attempt_no, status, started_at, finished_at, counters, error_code | dry-run/apply/verify/rollback/recovery/archive各呼出しのsupertype。行を再利用しない |
| `execution_runs` | id FK workflow_attempts, plan_run_id, mode(dry_run/apply), lease_fencing_token | Plan実行attempt |
| `rollback_runs` | id FK workflow_attempts, execution_run_id, mode(dry_run/apply), lease_fencing_token | rollback attempt。以前のrollback logを上書きしない |
| `recovery_runs` | id FK workflow_attempts, execution_run_id, action, lease_fencing_token | recovery dry-run/実行の証跡 |
| `verify_runs` | id FK workflow_attempts, subject_kind, execution_run_id NULL, rollback_run_id NULL, recovery_run_id NULL | CHECKでsubject kindに対応するFKがちょうど1つ |
| `preflight_runs` | id, attempt_id, policy_version, started_at, finished_at, status | dry-runと本実行で共通engineを使った証拠 |
| `preflight_logs` | preflight_run_id, sequence_no, plan_item_id, severity, code, observed_identity/stat/hash, target_observation, payload_json/version | itemごとの現在状態。owner attemptを跨いで流用しない |
| `operation_journal` | id, mutation_attempt_id, plan_item_id, direction, sequence_no, `strategy`, state, transition_no, source/target/temp lossless path列, expected identity/stat/hash, staged/published identity/hash, fencing_token, error_code | write-ahead intentとcrash recoveryの権威。schema v10でstrategyと`staged_file_identity`を追加 |
| `operation_journal_events` | operation_id, transition_no, from_state, to_state, fencing_token, payload_json/version, created_at | append-only state遷移。`(operation_id, transition_no)` unique |
| `operation_logs` | execution_run_id, operation_id, sequence_no, result, error_code, payload_json/version | Apply表示用terminal summary。rollbackの根拠にはしない |
| `rollback_logs` | rollback_run_id, operation_id, source_operation_id, sequence_no, result, error_code | rollback attempt自身が所有 |
| `verify_logs` | verify_run_id, sequence_no, source_operation_id, expected_state, observed_state, result, error_code | VerifySubjectごとの期待値を保存 |
| `run_metrics` | attempt_id, phase, sample_no, elapsed_ms, item_count, bytes | attempt単位のphase計測 |
| `event_logs` | root_scan_id, attempt_id NULL, class, level, event_code, payload_json/version, redaction_class, created_at | UI/診断。retention class付き |

`operation_journal.state` はcopy strategyでは `prepared -> staging -> staged -> content_verified -> published -> source_deleted -> completed`、same-volume atomic strategyではstagingを明示的に省略して `prepared -> content_verified -> published -> source_deleted -> completed` の前進だけを許可し、terminalな `failed/manual_intervention` と未解決の `recovery_required` を持つ。各遷移は `(id, transition_no, current state, fencing_token)` のcompare-and-swapで更新し、対応eventを同じtransactionでinsertする。`prepared`はfilesystem変更前にcommitする。同一volumeはatomic no-replace rename、異volumeはexclusive temp copy + content verification + atomic no-replace publishをjournalのstrategy列で区別する。rollback/recoveryも同じjournalを新operationとして使い、元operation IDを参照する。

主要index/制約は `workflow_attempts(root_scan_id,started_at,id)`、`operation_journal(mutation_attempt_id,sequence_no)` unique、`operation_journal(state)`、`operation_journal_events(operation_id,transition_no)` unique、`preflight_logs(preflight_run_id,sequence_no)`、`verify_logs(verify_run_id,sequence_no)`、`rollback_logs(rollback_run_id,sequence_no)` とする。attemptのcounterはcacheであり、log/journalとの整合性をrepository testで検証する。

### Lease、history、archive、retention

| Table | 主な列 | 不変条件・用途 |
|---|---|---|
| `mutation_leases` | scope_key unique, lossless scope path, owner_id, fencing_token, heartbeat_at, expires_at | 一つのtokenがsource/target複数rowを所有。包含判定はCoreのWindows path規則を使い、全row heartbeat/releaseとjournal fencingを一致させる |
| `history_archives` | id, root_scan_id, archive_schema_version, manifest_path_encoding/blob/display, digest_algorithm/digest, record_count, byte_count, status, created_at, verified_at | native path codecで復元するarchive packageの検証済みmanifest。displayは権威にしない |
| `history_archive_members` | archive_id, entity_kind, entity_id, record_count | workflow graphの包含証跡 |
| `retention_runs` | id, policy_version, cutoff_by_class_json, deleted_counts_json, started_at, finished_at, status | diagnostic prune/purge自体の監査 |

history queryはactiveとarchivedを区別し、既定ではarchivedを折り畳む。整理はまずversion付きmanifestとcanonical JSONLをstream出力し、archive fileのatomic publishとdigest/record count検証後に同一transactionで `history_archives(status='verified')` と対象graphのarchive markerを確定する。active DBからのpurgeは別の明示操作であり、現行archive schemaのverified manifestがない、runがactive、lease中、またはrecovery_requiredなら拒否する。旧schemaのcompleted manifestはidempotency/purge根拠にせず現行schemaで再archiveし、対応版より新しいschemaは安全側に拒否する。

retentionはprogress/debug 7日、通常diagnostic 30日、failed/recovery関連180日を削除不可の既定下限とする。byte上限超過時も期間未到達、protected、active、未終端journal、recovery evidenceをevictせず、削除延期とcapacity超過を返す。operation journal/events、preflight/verify/rollback audit、archive manifestは対応workflowが現行schemaのverified archiveへ移るまでpruneしない。各maintenance transactionはclass別削除件数、削除理由、capacity状態を `retention_runs` と監査eventへ残す。
