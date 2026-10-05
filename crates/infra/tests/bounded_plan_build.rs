use music_folder_core::{
    ports::{ManualTargetChange, PlanBuildStore, PlanStore, ScanStore, StagedPlanItem},
    usecases::{PlanOptions, PlanUseCase, RevisePlanUseCase},
    DuplicateStrategy, FileFingerprint, FileKind, NamingRules, PlanAction, PlanItem, Risk,
    RunStatus, ScannedFile, TrackMetadata,
};
use music_folder_infra::sqlite::SqliteScanStore;
use rusqlite::{params, Connection};
use std::{path::Path, sync::Arc, time::Instant};
use tempfile::tempdir;
use uuid::Uuid;

fn metadata(title: String) -> TrackMetadata {
    TrackMetadata {
        artist: Some("Artist".into()),
        album_artist: Some("Artist".into()),
        album: Some("Album".into()),
        title: Some(title),
        track_no: Some(1),
        disc_no: Some(1),
        year: Some(2026),
        genre: None,
        has_artwork: None,
    }
}

fn seed_scan(store: &SqliteScanStore, source: &Path, count: usize) -> String {
    let scan_id = store.begin_scan(source).unwrap();
    for start in (0..count).step_by(127) {
        let end = (start + 127).min(count);
        let files = (start..end)
            .map(|index| ScannedFile {
                id: Uuid::new_v4(),
                path: source.join(format!("track-{index:06}.flac")),
                fingerprint: FileFingerprint {
                    size_bytes: index as u64 + 1,
                    mtime_ns: index as i128 + 10,
                    content_sha256: Some(format!("hash-{index:06}")),
                    file_identity: Some(format!("identity-{index:06}")),
                    version: 1,
                },
                metadata: Some(metadata("Same Title".into())),
                kind: FileKind::Music,
            })
            .collect::<Vec<_>>();
        store.save_batch(&scan_id, &files).unwrap();
    }
    store
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    scan_id
}

fn build_plan(
    store: Arc<SqliteScanStore>,
    scan_id: &str,
    target: &Path,
    batch_size: usize,
) -> String {
    PlanUseCase { store }
        .execute(
            scan_id,
            &PlanOptions {
                target_root: target.to_path_buf(),
                batch_size,
                naming: NamingRules {
                    filename_template: "{title}{extension}".into(),
                    duplicate_strategy: DuplicateStrategy::Sequence,
                    ..NamingRules::default()
                },
            },
        )
        .unwrap()
        .plan_id
}

fn plan_evidence(database: &Path, plan_id: &str) -> (String, i64, Vec<String>) {
    let connection = Connection::open(database).unwrap();
    let (hash, peak): (String, i64) = connection
        .query_row(
            "SELECT plan.snapshot_hash,build.peak_batch_items
               FROM plan_runs plan JOIN plan_builds build ON build.plan_id=plan.id
              WHERE plan.id=?1",
            params![plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let targets = connection
        .prepare("SELECT target_path FROM plan_items WHERE plan_id=?1 ORDER BY ordinal")
        .unwrap()
        .query_map(params![plan_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    let scratch: i64 = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM plan_item_staging WHERE build_id=?1) +
                (SELECT COUNT(*) FROM plan_music_anchor_staging WHERE build_id=?1) +
                (SELECT COUNT(*) FROM plan_target_allocations WHERE build_id=?1)",
            params![plan_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(scratch, 0, "completed builds must release all scratch rows");
    (hash, peak, targets)
}

#[test]
fn high_cardinality_plan_is_page_boundary_deterministic_and_batch_bounded() {
    // ponytail: cross the 512-item revision page with an override and a following row;
    // larger workloads belong in bounded_plan_twenty_thousand_item_benchmark.
    const ITEM_COUNT: usize = 514;
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("bounded-plan.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = seed_scan(store.as_ref(), &temporary.path().join("source"), ITEM_COUNT);
    let target = temporary.path().join("target");

    let tiny_plan = build_plan(Arc::clone(&store), &scan_id, &target, 7);
    let wide_plan = build_plan(Arc::clone(&store), &scan_id, &target, 113);
    let (tiny_hash, tiny_peak, tiny_targets) = plan_evidence(&database, &tiny_plan);
    let (wide_hash, wide_peak, wide_targets) = plan_evidence(&database, &wide_plan);

    assert_eq!(tiny_targets.len(), ITEM_COUNT);
    assert_eq!(tiny_targets, wide_targets);
    assert_eq!(tiny_hash, wide_hash);
    assert!(tiny_peak <= 7, "observed peak page was {tiny_peak}");
    assert!(wide_peak <= 113, "observed peak page was {wide_peak}");
    assert!(tiny_targets[0].ends_with("Same Title.flac"));
    assert!(tiny_targets[1].ends_with("Same Title_2.flac"));
    assert!(tiny_targets[ITEM_COUNT - 1].ends_with("Same Title_514.flac"));

    let revised_item_id: String = Connection::open(&database)
        .unwrap()
        .query_row(
            "SELECT id FROM plan_items WHERE plan_id=?1 AND ordinal=513",
            params![wide_plan],
            |row| row.get(0),
        )
        .unwrap();
    let child_plan = RevisePlanUseCase {
        store: Arc::clone(&store),
    }
    .execute(
        &wide_plan,
        &[ManualTargetChange {
            plan_item_id: revised_item_id,
            target: target.join("manual.flac"),
            reason: "bounded-revision-test".into(),
        }],
    )
    .unwrap();
    let (child_hash, child_peak, child_targets) = plan_evidence(&database, &child_plan);
    assert_eq!(child_targets.len(), ITEM_COUNT);
    assert!(child_targets[512].ends_with("manual.flac"));
    assert_eq!(child_targets[513], wide_targets[513]);
    assert_ne!(child_hash, wide_hash);
    assert!(child_peak <= 512, "observed revision peak was {child_peak}");
    let connection = Connection::open(&database).unwrap();
    let (revision_rows, manual_rows): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),SUM(origin='manual') FROM plan_revision_changes
              WHERE child_plan_id=?1",
            params![child_plan],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision_rows, ITEM_COUNT as i64);
    assert_eq!(manual_rows, 1);
}

#[test]
fn conflict_groups_are_identical_when_the_collision_crosses_every_page_boundary() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("page-boundary-conflict.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = seed_scan(store.as_ref(), &temporary.path().join("source"), 41);
    let target = temporary.path().join("target");
    let run = |batch_size| {
        PlanUseCase {
            store: Arc::clone(&store),
        }
        .execute(
            &scan_id,
            &PlanOptions {
                target_root: target.clone(),
                batch_size,
                naming: NamingRules {
                    filename_template: "{title}{extension}".into(),
                    duplicate_strategy: DuplicateStrategy::Skip,
                    ..NamingRules::default()
                },
            },
        )
        .unwrap()
        .plan_id
    };
    let one_item_pages = run(1);
    let wide_pages = run(37);
    let (one_hash, one_peak, one_targets) = plan_evidence(&database, &one_item_pages);
    let (wide_hash, wide_peak, wide_targets) = plan_evidence(&database, &wide_pages);
    assert_eq!(one_hash, wide_hash);
    assert_eq!(one_targets, wide_targets);
    assert!(one_peak <= 1);
    assert!(wide_peak <= 37);

    for plan_id in [one_item_pages, wide_pages] {
        let connection = Connection::open(&database).unwrap();
        let (conflicts, groups): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*),COUNT(DISTINCT conflict_group_id)
                   FROM plan_items
                  WHERE plan_id=?1 AND action='skip' AND risk='conflict'",
                params![plan_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(conflicts, 41);
        assert_eq!(groups, 1);
    }
}

#[test]
fn failed_and_cancelled_builds_remove_scratch_and_partial_plan_rows() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("aborted-plan.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = seed_scan(&store, &temporary.path().join("source"), 1);
    for status in [RunStatus::Failed, RunStatus::Cancelled] {
        let plan_id = store
            .begin_plan(
                &scan_id,
                &temporary.path().join("target"),
                &NamingRules::default(),
            )
            .unwrap();
        let staged = StagedPlanItem {
            item: PlanItem {
                id: Uuid::new_v4(),
                conflict_group_id: None,
                ordinal: 1,
                file: ScannedFile {
                    id: Uuid::new_v4(),
                    path: temporary.path().join("source/one.flac"),
                    fingerprint: FileFingerprint::legacy(1, 1),
                    metadata: Some(metadata("One".into())),
                    kind: FileKind::Music,
                },
                target: Some(temporary.path().join("target/one.flac")),
                action: PlanAction::Move,
                disposition: music_folder_core::ExecutionDisposition::Executable,
                risk: Risk::None,
                reason: None,
                issues: Vec::new(),
                conflict_candidates: Vec::new(),
            },
            parent_plan_item_id: None,
            change_origin: None,
            change_reason: None,
        };
        store.stage_plan_batch(&plan_id, &[staged], &[]).unwrap();
        store.abort_plan_build(&plan_id, status).unwrap();

        let connection = Connection::open(&database).unwrap();
        let (plan_status, build_status, scratch, published): (String, String, i64, i64) =
            connection
                .query_row(
                    "SELECT plan.status,build.status,
                            (SELECT COUNT(*) FROM plan_item_staging WHERE build_id=?1),
                            (SELECT COUNT(*) FROM plan_items WHERE plan_id=?1)
                       FROM plan_runs plan JOIN plan_builds build ON build.plan_id=plan.id
                      WHERE plan.id=?1",
                    params![plan_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
        let expected = if status == RunStatus::Failed {
            "failed"
        } else {
            "cancelled"
        };
        assert_eq!(plan_status, expected);
        assert_eq!(build_status, expected);
        assert_eq!(scratch, 0);
        assert_eq!(published, 0);
    }
}

#[test]
fn adapter_revalidates_executable_safe_target_before_staging() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("unsafe-staging.db");
    let store = SqliteScanStore::open(&database).unwrap();
    let scan_id = seed_scan(&store, &temporary.path().join("source"), 1);
    let target_root = temporary.path().join("target");
    let plan_id = store
        .begin_plan(&scan_id, &target_root, &NamingRules::default())
        .unwrap();
    let file = store.load_completed_scan(&scan_id).unwrap().remove(0);
    let staged = StagedPlanItem {
        item: PlanItem {
            id: Uuid::new_v4(),
            conflict_group_id: None,
            ordinal: 1,
            file,
            target: Some(temporary.path().join("outside/escaped.flac")),
            action: PlanAction::Move,
            disposition: music_folder_core::ExecutionDisposition::Executable,
            risk: Risk::None,
            reason: None,
            issues: Vec::new(),
            conflict_candidates: Vec::new(),
        },
        parent_plan_item_id: None,
        change_origin: None,
        change_reason: None,
    };
    let error = store
        .stage_plan_batch(&plan_id, &[staged], &[])
        .unwrap_err();
    assert!(error.starts_with("plan_item_target_unsafe:"), "{error}");
}

#[test]
#[ignore = "manual performance evidence: creates 20,000 deterministic Plan items"]
fn bounded_plan_twenty_thousand_item_benchmark() {
    const ITEM_COUNT: usize = 20_000;
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("bounded-plan-benchmark.db");
    let store = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = seed_scan(store.as_ref(), &temporary.path().join("source"), ITEM_COUNT);
    let started = Instant::now();
    let plan_id = build_plan(store, &scan_id, &temporary.path().join("target"), 512);
    let (_, peak, targets) = plan_evidence(&database, &plan_id);
    eprintln!(
        "bounded_plan items={} elapsed_ms={} peak_batch_items={}",
        targets.len(),
        started.elapsed().as_millis(),
        peak
    );
    assert_eq!(targets.len(), ITEM_COUNT);
    assert!(peak <= 512);
}
