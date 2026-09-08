use std::path::PathBuf;

use crate::path_codec::LosslessPathEnvelope;

#[derive(Debug, serde::Serialize)]
pub struct HistoryRow {
    pub id: String,
    pub kind: String,
    pub mode: Option<String>,
    pub logical_run_id: String,
    pub attempt_no: u64,
    pub predecessor_attempt_id: Option<String>,
    pub actor: String,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub parent_id: Option<String>,
    pub root_scan_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
    pub error_code: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct RunDetailRow {
    pub id: String,
    pub kind: String,
    pub logical_run_id: String,
    pub attempt_no: u64,
    pub predecessor_attempt_id: Option<String>,
    pub actor: String,
    pub status: String,
    pub parent_id: Option<String>,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
    pub error_code: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanItemRow {
    pub id: String,
    pub conflict_group_id: Option<String>,
    pub conflict_member_count: u64,
    pub ordinal: u64,
    pub source_path: String,
    pub source: LosslessPathEnvelope,
    pub target_path: Option<String>,
    pub target: Option<LosslessPathEnvelope>,
    pub action: String,
    pub risk: String,
    pub reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanConflictMemberRow {
    pub item_id: String,
    pub ordinal: u64,
    pub source_path: String,
    pub source: LosslessPathEnvelope,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanConflictDetail {
    pub id: String,
    pub kind: String,
    pub target_path: String,
    pub target: Option<LosslessPathEnvelope>,
    pub existing_target_path: Option<String>,
    pub existing_target: Option<LosslessPathEnvelope>,
    pub members: Vec<PlanConflictMemberRow>,
    pub candidates: Vec<PlanConflictCandidateRow>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanConflictCandidateRow {
    pub ordinal: u64,
    pub target_path: String,
    pub target: LosslessPathEnvelope,
    pub members: Vec<PlanConflictMemberRow>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct PlanItemCounts {
    pub moves: u64,
    pub skips: u64,
    pub needs_attention: u64,
    pub conflicts: u64,
    pub invalid_target: u64,
    pub metadata_missing: u64,
    pub path_too_long: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanItemPage {
    pub items: Vec<PlanItemRow>,
    pub total: u64,
    pub filtered_total: u64,
    pub next_cursor: Option<u64>,
    pub counts: PlanItemCounts,
}

#[derive(serde::Serialize)]
pub struct OperationLogRow {
    pub id: String,
    pub execution_id: String,
    pub sequence_no: u64,
    pub source_path: String,
    pub source: LosslessPathEnvelope,
    pub target_path: Option<String>,
    pub target: Option<LosslessPathEnvelope>,
    pub action: String,
    pub result: String,
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct AttemptPreflightRow {
    pub id: String,
    pub kind: String,
    pub mode: String,
    pub status: String,
    pub passed: u64,
    pub blocked: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct AttemptJournalRow {
    pub id: String,
    pub sequence_no: u64,
    pub direction: String,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct AttemptLogRow {
    pub sequence_no: u64,
    pub phase: String,
    pub result: String,
    pub error: Option<String>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct AttemptEvidence {
    pub preflights: Vec<AttemptPreflightRow>,
    pub journal: Vec<AttemptJournalRow>,
    pub logs: Vec<AttemptLogRow>,
}

#[derive(serde::Serialize)]
pub struct MetricRow {
    pub phase: String,
    pub elapsed_ms: u64,
    pub item_count: u64,
}

#[derive(serde::Serialize)]
pub struct HistoryCleanupPreview {
    pub root_scan_id: String,
    pub plans: u64,
    pub executions: u64,
    pub logs: u64,
    pub blocked: bool,
    pub protection_reasons: Vec<String>,
    pub estimated_bytes: u64,
    #[serde(serialize_with = "crate::path_codec::serialize_archive_path")]
    pub archive_path: PathBuf,
    pub archived: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct HistoryArchiveResult {
    pub archive_id: String,
    pub root_scan_id: String,
    #[serde(serialize_with = "crate::path_codec::serialize_archive_path")]
    pub archive_path: PathBuf,
    pub schema_version: u32,
    pub record_count: u64,
    pub byte_count: u64,
    pub digest_algorithm: String,
    pub sha256: String,
    pub verified: bool,
    pub already_archived: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticEventInput {
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub class: String,
    pub severity: String,
    pub phase: String,
    pub code: String,
    pub item_id: Option<String>,
    pub sequence_no: Option<u64>,
    pub path_role: Option<String>,
    pub message_key: String,
    pub payload: serde_json::Value,
    pub contains_sensitive_path: bool,
    pub protected: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DiagnosticPresentation {
    pub summary_ja: String,
    pub impact_ja: String,
    pub next_action_ja: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticEventRow {
    pub correlation_id: String,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub class: String,
    pub severity: String,
    pub phase: String,
    pub code: String,
    pub item_id: Option<String>,
    pub sequence_no: Option<u64>,
    pub path_role: Option<String>,
    pub message_key: String,
    pub payload: serde_json::Value,
    pub contains_sensitive_path: bool,
    pub protected: bool,
    pub created_at: i64,
    pub presentation: DiagnosticPresentation,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticRetentionPolicy {
    pub progress_debug_days: u32,
    pub diagnostic_days: u32,
    pub recovery_audit_days: u32,
    pub batch_size: u32,
    pub max_database_bytes: Option<u64>,
}

impl Default for DiagnosticRetentionPolicy {
    fn default() -> Self {
        Self {
            progress_debug_days: 7,
            diagnostic_days: 30,
            recovery_audit_days: 180,
            batch_size: 1_000,
            max_database_bytes: None,
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct DiagnosticRetentionResult {
    pub retention_run_id: String,
    pub deleted_progress_debug: u64,
    pub deleted_diagnostic: u64,
    pub deleted_recovery_audit: u64,
    pub protected_remaining: u64,
    pub database_bytes: u64,
    pub capacity_exceeded: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct DiagnosticRetentionPreview {
    pub eligible_progress_debug: u64,
    pub eligible_diagnostic: u64,
    pub eligible_recovery_audit: u64,
    pub protected_remaining: u64,
    pub batch_size: u32,
    pub database_bytes: u64,
    pub capacity_exceeded: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct DiagnosticExportResult {
    #[serde(serialize_with = "crate::path_codec::serialize_diagnostic_export_path")]
    pub export_path: PathBuf,
    pub schema_version: u32,
    pub record_count: u64,
    pub byte_count: u64,
    pub sha256: String,
    pub sensitive_paths_redacted: bool,
}
