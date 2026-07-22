#[derive(Debug, serde::Serialize)]
pub struct HistoryRow {
    pub id: String,
    pub kind: String,
    pub mode: Option<String>,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub parent_id: Option<String>,
    pub root_scan_id: String,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct RunDetailRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub parent_id: Option<String>,
    pub success: u64,
    pub skipped: u64,
    pub failed: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanItemRow {
    pub id: String,
    pub conflict_group_id: Option<String>,
    pub conflict_member_count: u64,
    pub ordinal: u64,
    pub source_path: String,
    pub target_path: Option<String>,
    pub action: String,
    pub risk: String,
    pub reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanConflictMemberRow {
    pub item_id: String,
    pub ordinal: u64,
    pub source_path: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanConflictDetail {
    pub id: String,
    pub kind: String,
    pub target_path: String,
    pub existing_target_path: Option<String>,
    pub members: Vec<PlanConflictMemberRow>,
    pub candidates: Vec<PlanConflictCandidateRow>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlanConflictCandidateRow {
    pub target_path: String,
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
    pub target_path: Option<String>,
    pub action: String,
    pub result: String,
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(serde::Serialize)]
pub struct MetricRow {
    pub phase: String,
    pub elapsed_ms: u64,
    pub item_count: u64,
}

#[derive(serde::Serialize)]
pub struct HistoryCleanupPreview {
    pub plans: u64,
    pub executions: u64,
    pub logs: u64,
    pub blocked: bool,
}
