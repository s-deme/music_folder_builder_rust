use music_folder_core::usecases::{PlanOptions, PlanUseCase, RevisePlanUseCase};
use music_folder_core::{
    ports::{
        ApplyStore, ManualTargetChange, PlanBuildStore, PlanMusicAnchor, PlanRevisionBuild,
        PlanRevisionStore, PlanStore, ScanStore, StagedPlanItem,
    },
    scan_snapshot_hash, windows_path_key, ExecutionDisposition, FileFingerprint, FileKind,
    NamingRules, PlanAction, PlanIssue, PlanItem, Risk, RunStatus, ScannedFile, TrackMetadata,
};
use music_folder_infra::sqlite::SqliteScanStore;
use rusqlite::{params, Connection};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tempfile::tempdir;
use uuid::Uuid;

#[derive(Clone)]
struct MemoryPlan {
    scan_id: String,
    scan_snapshot_hash: String,
    target_root: PathBuf,
    naming: NamingRules,
    items: Vec<PlanItem>,
    snapshot_hash: String,
}

#[derive(Clone)]
struct MemoryBuild {
    plan_id: String,
    _parent_plan_id: Option<String>,
    scan_id: String,
    scan_snapshot_hash: String,
    target_root: PathBuf,
    naming: NamingRules,
    staged: BTreeMap<u64, StagedPlanItem>,
    original_keys: BTreeMap<u64, Option<String>>,
    anchors: Vec<PlanMusicAnchor>,
    allocations: HashSet<String>,
    published: BTreeMap<u64, PlanItem>,
}

#[derive(Default)]
struct MemoryState {
    scans: HashMap<String, Vec<ScannedFile>>,
    builds: HashMap<String, MemoryBuild>,
    plans: HashMap<String, MemoryPlan>,
    next_id: u64,
}

struct InMemoryPlanStore {
    state: Mutex<MemoryState>,
}

impl InMemoryPlanStore {
    fn new(scan_id: String, mut files: Vec<ScannedFile>) -> Self {
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut state = MemoryState::default();
        state.scans.insert(scan_id, files);
        Self {
            state: Mutex::new(state),
        }
    }

    fn create_build(
        state: &mut MemoryState,
        scan_id: &str,
        target_root: &Path,
        naming: &NamingRules,
        parent_plan_id: Option<String>,
    ) -> Result<String, String> {
        let files = state
            .scans
            .get(scan_id)
            .ok_or_else(|| "scan_not_completed".to_owned())?;
        state.next_id += 1;
        let plan_id = format!("memory-plan-{}", state.next_id);
        state.builds.insert(
            plan_id.clone(),
            MemoryBuild {
                plan_id: plan_id.clone(),
                _parent_plan_id: parent_plan_id,
                scan_id: scan_id.to_owned(),
                scan_snapshot_hash: scan_snapshot_hash(files),
                target_root: target_root.to_path_buf(),
                naming: naming.clone(),
                staged: BTreeMap::new(),
                original_keys: BTreeMap::new(),
                anchors: Vec::new(),
                allocations: HashSet::new(),
                published: BTreeMap::new(),
            },
        );
        Ok(plan_id)
    }

    fn plan(&self, plan_id: &str) -> MemoryPlan {
        self.state.lock().unwrap().plans[plan_id].clone()
    }
}

fn target_key(item: &PlanItem) -> Option<String> {
    item.target.as_deref().map(windows_path_key)
}

impl PlanBuildStore for InMemoryPlanStore {
    fn stage_plan_batch(
        &self,
        build_id: &str,
        items: &[StagedPlanItem],
        anchors: &[PlanMusicAnchor],
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let build = state
            .builds
            .get_mut(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        for staged in items {
            build
                .original_keys
                .insert(staged.item.ordinal, target_key(&staged.item));
            build.staged.insert(staged.item.ordinal, staged.clone());
        }
        build.anchors.extend_from_slice(anchors);
        Ok(())
    }

    fn load_staged_plan_page(
        &self,
        build_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<StagedPlanItem>, String> {
        let state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        Ok(build
            .staged
            .range((after_ordinal + 1)..)
            .take(limit.max(1))
            .map(|(_, staged)| staged.clone())
            .collect())
    }

    fn update_staged_plan_batch(
        &self,
        build_id: &str,
        items: &[StagedPlanItem],
        update_original_target: bool,
        allocated_target_keys: &[String],
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let build = state
            .builds
            .get_mut(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        for staged in items {
            if update_original_target {
                build
                    .original_keys
                    .insert(staged.item.ordinal, target_key(&staged.item));
            }
            build.staged.insert(staged.item.ordinal, staged.clone());
        }
        build
            .allocations
            .extend(allocated_target_keys.iter().cloned());
        Ok(())
    }

    fn load_nearest_music_anchors(
        &self,
        build_id: &str,
        source_ancestors: &[PathBuf],
    ) -> Result<Vec<PlanMusicAnchor>, String> {
        let state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        Ok(build
            .anchors
            .iter()
            .filter(|anchor| source_ancestors.contains(&anchor.source_directory))
            .cloned()
            .collect())
    }

    fn original_target_rank(
        &self,
        build_id: &str,
        normalized_target_key: &str,
        ordinal: u64,
    ) -> Result<u64, String> {
        let state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        Ok(build
            .original_keys
            .range(..=ordinal)
            .filter(|(_, key)| key.as_deref() == Some(normalized_target_key))
            .count() as u64)
    }

    fn original_target_exists_after(
        &self,
        build_id: &str,
        normalized_target_key: &str,
        ordinal: u64,
    ) -> Result<bool, String> {
        let state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        Ok(build
            .original_keys
            .range((ordinal + 1)..)
            .any(|(_, key)| key.as_deref() == Some(normalized_target_key)))
    }

    fn target_is_allocated(
        &self,
        build_id: &str,
        normalized_target_key: &str,
    ) -> Result<bool, String> {
        let state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        Ok(build.allocations.contains(normalized_target_key))
    }

    fn staged_target_count(
        &self,
        build_id: &str,
        normalized_target_key: &str,
    ) -> Result<u64, String> {
        let state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        Ok(build
            .staged
            .values()
            .filter(|staged| target_key(&staged.item).as_deref() == Some(normalized_target_key))
            .count() as u64)
    }

    fn staged_plan_item_count(&self, build_id: &str) -> Result<u64, String> {
        let state = self.state.lock().unwrap();
        state
            .builds
            .get(build_id)
            .map(|build| build.staged.len() as u64)
            .ok_or_else(|| "plan_build_not_active".to_owned())
    }

    fn publish_staged_plan_page(
        &self,
        build_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<PlanItem>, String> {
        let mut state = self.state.lock().unwrap();
        let build = state
            .builds
            .get_mut(build_id)
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        let page = build
            .staged
            .range((after_ordinal + 1)..)
            .take(limit.max(1))
            .map(|(_, staged)| staged.item.clone())
            .collect::<Vec<_>>();
        for item in &page {
            build.published.insert(item.ordinal, item.clone());
        }
        Ok(page)
    }

    fn complete_plan_build(
        &self,
        build_id: &str,
        _conflict_count: u64,
        _risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let build = state
            .builds
            .get(build_id)
            .cloned()
            .ok_or_else(|| "plan_build_not_active".to_owned())?;
        state.plans.insert(
            build.plan_id.clone(),
            MemoryPlan {
                scan_id: build.scan_id,
                scan_snapshot_hash: build.scan_snapshot_hash,
                target_root: build.target_root,
                naming: build.naming,
                items: build.published.into_values().collect(),
                snapshot_hash: snapshot_hash.to_owned(),
            },
        );
        Ok(())
    }

    fn abort_plan_build(&self, build_id: &str, _status: RunStatus) -> Result<(), String> {
        self.state.lock().unwrap().builds.remove(build_id);
        Ok(())
    }
}

impl PlanStore for InMemoryPlanStore {
    fn load_completed_scan(&self, scan_id: &str) -> Result<Vec<ScannedFile>, String> {
        self.state
            .lock()
            .unwrap()
            .scans
            .get(scan_id)
            .cloned()
            .ok_or_else(|| "scan_not_completed".to_owned())
    }

    fn load_completed_scan_page(
        &self,
        scan_id: &str,
        after_path: Option<&Path>,
        limit: usize,
    ) -> Result<Vec<ScannedFile>, String> {
        Ok(self
            .load_completed_scan(scan_id)?
            .into_iter()
            .filter(|file| after_path.is_none_or(|after| file.path.as_path() > after))
            .take(limit.max(1))
            .collect())
    }

    fn completed_scan_snapshot_hash(&self, scan_id: &str) -> Result<String, String> {
        Ok(scan_snapshot_hash(&self.load_completed_scan(scan_id)?))
    }

    fn begin_plan(
        &self,
        scan_id: &str,
        target_root: &Path,
        naming: &NamingRules,
    ) -> Result<String, String> {
        Self::create_build(
            &mut self.state.lock().unwrap(),
            scan_id,
            target_root,
            naming,
            None,
        )
    }

    fn save_plan_items(&self, _plan_id: &str, _items: &[PlanItem]) -> Result<(), String> {
        Err("bounded_plan_build_required".into())
    }

    fn finish_plan(
        &self,
        plan_id: &str,
        conflict_count: u64,
        risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String> {
        self.complete_plan_build(plan_id, conflict_count, risk_count, snapshot_hash)
    }

    fn fail_plan(&self, plan_id: &str) -> Result<(), String> {
        self.abort_plan_build(plan_id, RunStatus::Failed)
    }
}

impl PlanRevisionStore for InMemoryPlanStore {
    fn begin_plan_revision(&self, parent_plan_id: &str) -> Result<PlanRevisionBuild, String> {
        let mut state = self.state.lock().unwrap();
        let parent = state
            .plans
            .get(parent_plan_id)
            .cloned()
            .ok_or_else(|| "parent_plan_not_completed".to_owned())?;
        let child_plan_id = Self::create_build(
            &mut state,
            &parent.scan_id,
            &parent.target_root,
            &parent.naming,
            Some(parent_plan_id.to_owned()),
        )?;
        Ok(PlanRevisionBuild {
            child_plan_id: child_plan_id.clone(),
            build_id: child_plan_id,
            scan_id: parent.scan_id,
            scan_snapshot_hash: parent.scan_snapshot_hash,
            target_root: parent.target_root,
            naming: parent.naming,
        })
    }

    fn load_parent_plan_page(
        &self,
        parent_plan_id: &str,
        after_ordinal: u64,
        limit: usize,
    ) -> Result<Vec<StagedPlanItem>, String> {
        let state = self.state.lock().unwrap();
        let plan = state
            .plans
            .get(parent_plan_id)
            .ok_or_else(|| "parent_plan_not_completed".to_owned())?;
        Ok(plan
            .items
            .iter()
            .filter(|item| item.ordinal > after_ordinal)
            .take(limit.max(1))
            .cloned()
            .map(|item| StagedPlanItem {
                parent_plan_item_id: Some(item.id.to_string()),
                item,
                change_origin: None,
                change_reason: None,
            })
            .collect())
    }
}

type Projection = (
    u64,
    Option<PathBuf>,
    PlanAction,
    ExecutionDisposition,
    Risk,
    Option<String>,
    Vec<PlanIssue>,
);

fn memory_projection(plan: &MemoryPlan) -> Vec<Projection> {
    plan.items
        .iter()
        .map(|item| {
            (
                item.ordinal,
                item.target.clone(),
                item.action,
                item.disposition,
                item.risk,
                item.reason.clone(),
                item.issues.clone(),
            )
        })
        .collect()
}

fn sqlite_evidence(
    store: &SqliteScanStore,
    database: &Path,
    plan_id: &str,
) -> (String, Vec<Projection>, Vec<String>) {
    store.validate_plan_snapshot(plan_id).unwrap();
    let hash = Connection::open(database)
        .unwrap()
        .query_row(
            "SELECT snapshot_hash FROM plan_runs WHERE id=?1",
            params![plan_id],
            |row| row.get(0),
        )
        .unwrap();
    let items = store.load_completed_plan(plan_id).unwrap();
    let ids = items
        .iter()
        .map(|item| item.plan_item_id.clone())
        .collect::<Vec<_>>();
    let projection = items
        .into_iter()
        .map(|item| {
            (
                item.ordinal,
                item.target,
                item.action,
                item.disposition,
                item.risk,
                item.reason,
                item.issues,
            )
        })
        .collect();
    (hash, projection, ids)
}

fn metadata(title: &str, track_no: u32) -> TrackMetadata {
    TrackMetadata {
        artist: Some("Artist".into()),
        album_artist: Some("Artist".into()),
        album: Some("Album".into()),
        title: Some(title.into()),
        track_no: Some(track_no),
        disc_no: Some(1),
        year: Some(2026),
        genre: None,
        has_artwork: None,
    }
}

#[test]
fn create_and_revise_have_identical_in_memory_and_sqlite_contracts() {
    let temporary = tempdir().unwrap();
    let database = temporary.path().join("plan-contract.db");
    let source_root = temporary.path().join("source");
    let target_root = temporary.path().join("target");
    let files = [
        ("one.flac", "Same", 1_u32),
        ("two.flac", "Same", 2_u32),
        ("three.flac", "Other", 3_u32),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, title, track_no))| ScannedFile {
        id: Uuid::from_u128(index as u128 + 1),
        path: source_root.join(name),
        fingerprint: FileFingerprint {
            size_bytes: index as u64 + 10,
            mtime_ns: index as i128 + 100,
            content_sha256: Some(format!("hash-{index}")),
            file_identity: Some(format!("identity-{index}")),
            version: 1,
        },
        metadata: Some(metadata(title, track_no)),
        kind: FileKind::Music,
    })
    .collect::<Vec<_>>();

    let sqlite = Arc::new(SqliteScanStore::open(&database).unwrap());
    let scan_id = sqlite.begin_scan(&source_root).unwrap();
    sqlite.save_batch(&scan_id, &files).unwrap();
    sqlite
        .finish_scan(&scan_id, RunStatus::Completed, 0)
        .unwrap();
    let memory = Arc::new(InMemoryPlanStore::new(scan_id.clone(), files));
    let options = PlanOptions {
        target_root: target_root.clone(),
        batch_size: 2,
        naming: NamingRules {
            filename_template: "{title}{extension}".into(),
            duplicate_strategy: music_folder_core::DuplicateStrategy::Sequence,
            ..NamingRules::default()
        },
    };

    let memory_plan_id = PlanUseCase {
        store: Arc::clone(&memory),
    }
    .execute(&scan_id, &options)
    .unwrap()
    .plan_id;
    let sqlite_plan_id = PlanUseCase {
        store: Arc::clone(&sqlite),
    }
    .execute(&scan_id, &options)
    .unwrap()
    .plan_id;
    let memory_plan = memory.plan(&memory_plan_id);
    let (sqlite_hash, sqlite_projection, sqlite_ids) =
        sqlite_evidence(sqlite.as_ref(), &database, &sqlite_plan_id);
    assert_eq!(memory_plan.snapshot_hash, sqlite_hash);
    assert_eq!(memory_projection(&memory_plan), sqlite_projection);

    let manual_target = target_root.join("Manual/renamed.flac");
    let memory_parent_id = memory_plan.items[1].id.to_string();
    let memory_child_id = RevisePlanUseCase {
        store: Arc::clone(&memory),
    }
    .execute(
        &memory_plan_id,
        &[ManualTargetChange {
            plan_item_id: memory_parent_id,
            target: manual_target.clone(),
            reason: "contract-manual".into(),
        }],
    )
    .unwrap();
    let sqlite_child_id = RevisePlanUseCase {
        store: Arc::clone(&sqlite),
    }
    .execute(
        &sqlite_plan_id,
        &[ManualTargetChange {
            plan_item_id: sqlite_ids[1].clone(),
            target: manual_target,
            reason: "contract-manual".into(),
        }],
    )
    .unwrap();
    let memory_child = memory.plan(&memory_child_id);
    let (sqlite_child_hash, sqlite_child_projection, _) =
        sqlite_evidence(sqlite.as_ref(), &database, &sqlite_child_id);
    assert_eq!(memory_child.snapshot_hash, sqlite_child_hash);
    assert_eq!(memory_projection(&memory_child), sqlite_child_projection);
}
