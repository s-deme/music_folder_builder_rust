use crate::{
    ApplyItem, FileFingerprint, OperationLog, PlanItem, ScannedFile, TrackMetadata, VerifyItem,
};
use std::path::Path;

pub trait MetadataReader: Send + Sync {
    fn read(&self, path: &Path) -> Result<TrackMetadata, String>;
}
pub trait ScanStore: Send + Sync {
    fn previous_metadata(
        &self,
        path: &Path,
        fingerprint: &FileFingerprint,
    ) -> Result<Option<TrackMetadata>, String>;
    fn begin_scan(&self, source: &Path) -> Result<String, String>;
    fn save_batch(&self, scan_id: &str, files: &[ScannedFile]) -> Result<(), String>;
    fn finish_scan(&self, scan_id: &str, status: &str, warnings: u64) -> Result<(), String>;
    fn save_scan_warning(&self, _scan_id: &str, _warning: &str) -> Result<(), String> {
        Ok(())
    }
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}
pub trait FileSystem: Send + Sync {
    /// Calls `visitor` once per candidate. Implementations must not accumulate the
    /// entire library in memory before calling the visitor.
    fn enumerate(
        &self,
        root: &Path,
        follow_links: bool,
        visitor: &mut dyn FnMut(Result<std::path::PathBuf, String>) -> bool,
    ) -> Result<(), String>;
    fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, String>;
}

pub trait PlanStore: Send + Sync {
    fn load_completed_scan(&self, scan_id: &str) -> Result<Vec<ScannedFile>, String>;
    fn begin_plan(&self, scan_id: &str, target_root: &Path) -> Result<String, String>;
    fn save_plan_items(&self, plan_id: &str, items: &[PlanItem]) -> Result<(), String>;
    fn finish_plan(
        &self,
        plan_id: &str,
        conflict_count: u64,
        risk_count: u64,
        snapshot_hash: &str,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

pub trait ApplyStore: Send + Sync {
    fn load_completed_plan(&self, plan_id: &str) -> Result<Vec<ApplyItem>, String>;
    /// Reject execution when the persisted plan rows no longer match their snapshot.
    fn validate_plan_snapshot(&self, plan_id: &str) -> Result<(), String>;
    /// Successful non-dry-run items are never mutated twice for the same plan.
    fn successful_plan_item_ids(&self, plan_id: &str) -> Result<Vec<String>, String>;
    fn begin_execution(&self, plan_id: &str, dry_run: bool) -> Result<String, String>;
    fn save_operation(&self, execution_id: &str, operation: &OperationLog) -> Result<(), String>;
    fn finish_execution(
        &self,
        execution_id: &str,
        status: &str,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

pub trait FileMutator: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn same_volume(&self, source: &Path, target: &Path) -> Result<bool, String>;
    fn move_file(&self, source: &Path, target: &Path) -> Result<(), String>;
    fn copy_file(&self, source: &Path, target: &Path) -> Result<(), String>;
    fn size(&self, path: &Path) -> Result<u64, String>;
    fn delete_file(&self, path: &Path) -> Result<(), String>;
}

pub trait VerifyStore: Send + Sync {
    fn begin_verify(&self, execution_id: &str) -> Result<String, String>;
    fn load_successful_operations(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String>;
    fn save_verify_result(
        &self,
        execution_id: &str,
        operation_id: &str,
        result: &str,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn finish_verify(
        &self,
        verify_id: &str,
        status: &str,
        success: u64,
        failed: u64,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}

pub trait RollbackStore: Send + Sync {
    fn begin_rollback(&self, execution_id: &str, dry_run: bool) -> Result<String, String>;
    fn load_rollback_items(&self, execution_id: &str) -> Result<Vec<VerifyItem>, String>;
    fn save_rollback_result(
        &self,
        execution_id: &str,
        operation_id: &str,
        result: &str,
        error: Option<&str>,
    ) -> Result<(), String>;
    fn finish_rollback(
        &self,
        rollback_id: &str,
        status: &str,
        success: u64,
        skipped: u64,
        failed: u64,
    ) -> Result<(), String>;
    fn record_metric(
        &self,
        _run_id: &str,
        _phase: &str,
        _elapsed_ms: u64,
        _item_count: u64,
    ) -> Result<(), String> {
        Ok(())
    }
}
