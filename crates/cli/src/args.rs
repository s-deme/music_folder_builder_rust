use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{ffi::OsString, path::PathBuf, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EventFormat {
    Jsonl,
}

#[derive(Debug, Parser)]
#[command(
    name = "music-folder",
    about = "Safe music library organizer",
    color = clap::ColorChoice::Never
)]
pub struct Cli {
    /// Selects human-readable text or a versioned JSON envelope.
    #[arg(long, global = true, value_enum, default_value = "human")]
    pub output: OutputFormat,
    /// Streams versioned machine-readable progress events to stderr.
    #[arg(long, global = true, value_enum)]
    pub events: Option<EventFormat>,
    /// Confirms a destructive command without an interactive TTY prompt.
    #[arg(long, global = true)]
    pub yes: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Scan {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, value_parser = parse_workers)]
        workers: Option<usize>,
    },
    /// Creates an immutable Plan. `plan revise` creates a child Plan.
    Plan {
        #[command(subcommand)]
        command: Option<PlanCommand>,
        #[command(flatten)]
        create: Box<PlanCreateArgs>,
    },
    Apply {
        #[arg(long, value_parser = parse_identifier)]
        plan_run_id: String,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        /// Actually mutate files. Without this flag, apply is a dry-run.
        #[arg(long)]
        execute: bool,
        /// Required for --execute and must exactly match --plan-run-id.
        #[arg(long, value_name = "PLAN_RUN_ID", value_parser = parse_identifier)]
        confirm: Option<String>,
    },
    Verify {
        /// Backward-compatible execution subject ID.
        #[arg(long, required_unless_present = "subject_id", value_parser = parse_identifier)]
        execution_run_id: Option<String>,
        /// Typed attempt kind to verify.
        #[arg(long, value_enum, default_value = "execution")]
        subject: VerifySubjectArg,
        /// Apply, rollback, or recovery attempt ID selected by --subject.
        #[arg(long, conflicts_with = "execution_run_id", value_parser = parse_identifier)]
        subject_id: Option<String>,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
    },
    Rollback {
        #[arg(long, value_parser = parse_identifier)]
        execution_run_id: String,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        /// Actually roll files back. Requires an exact matching --confirm value.
        #[arg(long)]
        execute: bool,
        #[arg(long, value_name = "EXECUTION_RUN_ID", value_parser = parse_identifier)]
        confirm: Option<String>,
    },
    /// Lists and cleans up persisted workflow history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Inspects or resolves non-terminal durable operation journals.
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
    /// Exports redacted diagnostics or runs bounded retention maintenance.
    Diagnostics {
        #[command(subcommand)]
        command: DiagnosticsCommand,
    },
    /// Measures three or more cold/warm Scan, Plan, and Apply dry-run iterations.
    Benchmark {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value = "music-folder-benchmark.db")]
        db: PathBuf,
        #[arg(long)]
        target: Option<PathBuf>,
        #[arg(long, value_parser = parse_workers)]
        workers: Option<usize>,
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(3..=10))]
        iterations: u32,
    },
    /// Emits a dependency-free completion script for the selected shell.
    Completions {
        #[arg(value_enum)]
        shell: CompletionShell,
    },
    /// Prints a compact command reference suitable for man-page generation.
    Man,
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Scan { .. } => "scan",
            Self::Plan {
                command: Some(PlanCommand::Revise { .. }),
                ..
            } => "plan.revise",
            Self::Plan { .. } => "plan",
            Self::Apply { .. } => "apply",
            Self::Verify { .. } => "verify",
            Self::Rollback { .. } => "rollback",
            Self::History { command } => command.name(),
            Self::Recovery { command } => command.name(),
            Self::Diagnostics { command } => command.name(),
            Self::Benchmark { .. } => "benchmark",
            Self::Completions { .. } => "completions",
            Self::Man => "man",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    Export {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        /// Explicitly include path-bearing payloads. Secret-like fields remain redacted.
        #[arg(long)]
        include_sensitive_paths: bool,
    },
    Retention {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u32).range(0..=36_500))]
        progress_debug_days: u32,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(0..=36_500))]
        diagnostic_days: u32,
        #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u32).range(0..=36_500))]
        recovery_audit_days: u32,
        #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u32).range(1..=10_000))]
        batch_size: u32,
        #[arg(long)]
        max_database_bytes: Option<u64>,
        /// Delete eligible diagnostic rows. Without this flag only a preview is returned.
        #[arg(long)]
        execute: bool,
    },
}

impl DiagnosticsCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Export { .. } => "diagnostics.export",
            Self::Retention { .. } => "diagnostics.retention",
        }
    }
}

#[derive(Debug, Args)]
pub struct PlanCreateArgs {
    #[arg(long, value_parser = parse_identifier)]
    pub scan_run_id: Option<String>,
    #[arg(long)]
    pub target: Option<PathBuf>,
    #[arg(long, default_value = "music-folder.db")]
    pub db: PathBuf,
    /// TOML file containing a `[naming]` section.
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, value_parser = parse_template)]
    pub artist_dir_template: Option<String>,
    #[arg(long, value_parser = parse_template)]
    pub album_dir_template: Option<String>,
    #[arg(long, value_parser = parse_template)]
    pub disc_dir_template: Option<String>,
    #[arg(long, value_parser = parse_template)]
    pub filename_template: Option<String>,
    #[arg(long, value_parser = parse_template)]
    pub duplicate_suffix_template: Option<String>,
    /// Duplicate handling: skip, sequence, or template.
    #[arg(long, value_enum)]
    pub duplicate_strategy: Option<DuplicateStrategyArg>,
    #[arg(long)]
    pub use_source_filename: bool,
    #[arg(long)]
    pub use_source_image_filename: bool,
    /// Create Unknown Artist/Unknown Album folders instead of skipping missing metadata.
    #[arg(long)]
    pub allow_missing_metadata: bool,
    /// Allows target paths beyond the default Windows 240-character policy.
    #[arg(long)]
    pub allow_long_paths: bool,
}

#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    Revise {
        #[arg(long, value_parser = parse_identifier)]
        plan_run_id: String,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        /// UTF-8 compatibility form. Prefer --manual for native lossless paths.
        #[arg(long = "change", value_name = "ITEM_ID=TARGET_PATH")]
        changes: Vec<TargetChange>,
        /// Lossless native-path form; repeat `--manual ITEM_ID TARGET_PATH` as needed.
        #[arg(
            long = "manual",
            value_names = ["ITEM_ID", "TARGET_PATH"],
            num_args = 2,
            action = clap::ArgAction::Append
        )]
        manual: Vec<OsString>,
        /// Opaque image destination selection: ITEM_ID:CONFLICT_GROUP_ID:ORDINAL.
        #[arg(long = "candidate", value_name = "ITEM_ID:GROUP_ID:ORDINAL")]
        candidates: Vec<CandidateSelection>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DuplicateStrategyArg {
    Skip,
    Sequence,
    Template,
}

impl From<DuplicateStrategyArg> for music_folder_core::DuplicateStrategy {
    fn from(value: DuplicateStrategyArg) -> Self {
        match value {
            DuplicateStrategyArg::Skip => Self::Skip,
            DuplicateStrategyArg::Sequence => Self::Sequence,
            DuplicateStrategyArg::Template => Self::Template,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetChange {
    pub item_id: String,
    pub target: PathBuf,
}

impl FromStr for TargetChange {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (item_id, target) = value
            .split_once('=')
            .ok_or_else(|| "change must be ITEM_ID=TARGET_PATH".to_string())?;
        if item_id.trim().is_empty() || target.trim().is_empty() {
            return Err("change must contain a non-empty item ID and target path".into());
        }
        let item_id = parse_identifier(item_id.trim())?;
        Ok(Self {
            item_id,
            target: PathBuf::from(target),
        })
    }
}

#[derive(Debug, Clone)]
pub struct CandidateSelection {
    pub item_id: String,
    pub conflict_group_id: String,
    pub ordinal: u64,
}

impl FromStr for CandidateSelection {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split(':');
        let item_id = parts
            .next()
            .ok_or_else(|| "candidate must be ITEM_ID:GROUP_ID:ORDINAL".to_string())?;
        let conflict_group_id = parts
            .next()
            .ok_or_else(|| "candidate must be ITEM_ID:GROUP_ID:ORDINAL".to_string())?;
        let ordinal = parts
            .next()
            .ok_or_else(|| "candidate must be ITEM_ID:GROUP_ID:ORDINAL".to_string())?;
        if parts.next().is_some() {
            return Err("candidate must be ITEM_ID:GROUP_ID:ORDINAL".into());
        }
        let ordinal = ordinal
            .parse::<u64>()
            .map_err(|_| "candidate ordinal must be an integer".to_string())?;
        if !(1..=10_000).contains(&ordinal) {
            return Err("candidate ordinal must be between 1 and 10000".into());
        }
        Ok(Self {
            item_id: parse_identifier(item_id)?,
            conflict_group_id: parse_identifier(conflict_group_id)?,
            ordinal,
        })
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RunKind {
    Scan,
    Plan,
    Apply,
    Verify,
    Rollback,
    Recovery,
    Archive,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum VerifySubjectArg {
    Execution,
    Rollback,
    Recovery,
}

impl VerifySubjectArg {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Rollback => "rollback",
            Self::Recovery => "recovery",
        }
    }
}

impl RunKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Plan => "plan",
            Self::Apply => "apply",
            Self::Verify => "verify",
            Self::Rollback => "rollback",
            Self::Recovery => "recovery",
            Self::Archive => "archive",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HistoryStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Partial,
    RecoveryRequired,
    Archived,
}

impl HistoryStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Partial => "partial",
            Self::RecoveryRequired => "recovery_required",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum HistoryCommand {
    List {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        #[arg(long, value_enum)]
        kind: Option<RunKind>,
        #[arg(long, value_enum)]
        status: Option<HistoryStatus>,
        #[arg(long, value_parser = parse_query)]
        query: Option<String>,
        #[arg(long)]
        oldest_first: bool,
    },
    CleanupPreview {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, value_enum)]
        kind: RunKind,
        #[arg(long, value_parser = parse_identifier)]
        run_id: String,
    },
    /// Writes and verifies a lossless JSONL archive, then marks the workflow archived.
    Archive {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, value_enum)]
        kind: RunKind,
        #[arg(long, value_parser = parse_identifier)]
        run_id: String,
        #[arg(long)]
        archive_dir: Option<PathBuf>,
        /// Must exactly match --run-id.
        #[arg(long, value_name = "RUN_ID", value_parser = parse_identifier)]
        confirm: Option<String>,
    },
    Delete {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, value_enum)]
        kind: RunKind,
        #[arg(long, value_parser = parse_identifier)]
        run_id: String,
        /// Must exactly match --run-id. This command permanently deletes DB history.
        #[arg(long, value_name = "RUN_ID", value_parser = parse_identifier)]
        confirm: Option<String>,
    },
}

impl HistoryCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List { .. } => "history.list",
            Self::CleanupPreview { .. } => "history.cleanup-preview",
            Self::Archive { .. } => "history.archive",
            Self::Delete { .. } => "history.delete",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RecoveryActionArg {
    Resume,
    RollbackPublished,
    DiscardUnpublishedTemporary,
}

impl From<RecoveryActionArg> for music_folder_core::RecoveryAction {
    fn from(value: RecoveryActionArg) -> Self {
        match value {
            RecoveryActionArg::Resume => Self::Resume,
            RecoveryActionArg::RollbackPublished => Self::RollbackPublished,
            RecoveryActionArg::DiscardUnpublishedTemporary => Self::DiscardUnpublishedTemporary,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    List {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
    },
    Run {
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long, value_parser = parse_identifier)]
        operation_id: String,
        #[arg(long, value_enum)]
        action: Option<RecoveryActionArg>,
        /// Executes the suggested/requested recovery action. The default is a dry-run.
        #[arg(long)]
        execute: bool,
        /// Required for --execute and must exactly match --operation-id.
        #[arg(long, value_name = "OPERATION_ID", value_parser = parse_identifier)]
        confirm: Option<String>,
    },
}

impl RecoveryCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List { .. } => "recovery.list",
            Self::Run { .. } => "recovery.run",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

pub fn requested_output(arguments: &[OsString]) -> OutputFormat {
    for (index, argument) in arguments.iter().enumerate() {
        let value = argument.to_string_lossy();
        if value == "--output=json" {
            return OutputFormat::Json;
        }
        if value == "--output"
            && arguments
                .get(index + 1)
                .is_some_and(|next| next.to_string_lossy() == "json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Human
}

pub fn requested_events(arguments: &[OsString]) -> Option<EventFormat> {
    for (index, argument) in arguments.iter().enumerate() {
        let value = argument.to_string_lossy();
        if value == "--events=jsonl"
            || (value == "--events"
                && arguments
                    .get(index + 1)
                    .is_some_and(|next| next.to_string_lossy() == "jsonl"))
        {
            return Some(EventFormat::Jsonl);
        }
    }
    None
}

pub fn parse_identifier(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 {
        return Err("identifier must contain between 1 and 128 UTF-8 bytes".into());
    }
    if value.chars().any(char::is_whitespace) {
        return Err("identifier must not contain whitespace".into());
    }
    Ok(value.to_owned())
}

fn parse_template(value: &str) -> Result<String, String> {
    if value.len() > 1_024 {
        Err("naming template must not exceed 1024 UTF-8 bytes".into())
    } else {
        Ok(value.to_owned())
    }
}

fn parse_query(value: &str) -> Result<String, String> {
    if value.len() > 256 {
        Err("history query must not exceed 256 UTF-8 bytes".into())
    } else {
        Ok(value.to_owned())
    }
}

fn parse_workers(value: &str) -> Result<usize, String> {
    let workers = value
        .parse::<usize>()
        .map_err(|_| "workers must be an integer between 1 and 256".to_string())?;
    if !(1..=256).contains(&workers) {
        return Err("workers must be between 1 and 256".into());
    }
    Ok(workers)
}

pub fn command_hint(arguments: &[OsString]) -> String {
    let values = arguments
        .iter()
        .skip(1)
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>();
    for (index, value) in values.iter().enumerate() {
        let nested = values.get(index + 1).map(|value| value.as_ref());
        match (value.as_ref(), nested) {
            ("plan", Some("revise")) => return "plan.revise".into(),
            ("history", Some("list")) => return "history.list".into(),
            ("history", Some("cleanup-preview")) => return "history.cleanup-preview".into(),
            ("history", Some("archive")) => return "history.archive".into(),
            ("history", Some("delete")) => return "history.delete".into(),
            ("recovery", Some("list")) => return "recovery.list".into(),
            ("recovery", Some("run")) => return "recovery.run".into(),
            ("diagnostics", Some("export")) => return "diagnostics.export".into(),
            ("diagnostics", Some("retention")) => return "diagnostics.retention".into(),
            (
                "scan" | "plan" | "apply" | "verify" | "rollback" | "history" | "recovery"
                | "diagnostics" | "benchmark" | "completions" | "man",
                _,
            ) => return value.to_string(),
            _ => {}
        }
    }
    "cli".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_selection_is_typed_and_bounded() {
        let candidate = "item-1:group-1:42"
            .parse::<CandidateSelection>()
            .expect("valid candidate");
        assert_eq!(candidate.item_id, "item-1");
        assert_eq!(candidate.conflict_group_id, "group-1");
        assert_eq!(candidate.ordinal, 42);
        assert!("item-1:group-1:0".parse::<CandidateSelection>().is_err());
        assert!("item-1:group-1:10001"
            .parse::<CandidateSelection>()
            .is_err());
        assert!("item-1:group-1:1:extra"
            .parse::<CandidateSelection>()
            .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn manual_revision_preserves_native_windows_path_units() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let native = OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xd800,
            b'.' as u16,
            b'm' as u16,
            b'p' as u16,
            b'3' as u16,
        ]);
        let cli = Cli::try_parse_from([
            OsString::from("music-folder"),
            OsString::from("plan"),
            OsString::from("revise"),
            OsString::from("--plan-run-id"),
            OsString::from("plan-1"),
            OsString::from("--manual"),
            OsString::from("item-1"),
            native.clone(),
        ])
        .expect("native manual target must parse without UTF-8 conversion");
        let Command::Plan {
            command: Some(PlanCommand::Revise { manual, .. }),
            ..
        } = cli.command
        else {
            panic!("expected plan revise")
        };
        assert_eq!(
            manual[1].encode_wide().collect::<Vec<_>>(),
            native.encode_wide().collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn manual_revision_preserves_native_unix_path_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let native = OsString::from_vec(b"/tmp/manual-\xff.mp3".to_vec());
        let cli = Cli::try_parse_from([
            OsString::from("music-folder"),
            OsString::from("plan"),
            OsString::from("revise"),
            OsString::from("--plan-run-id"),
            OsString::from("plan-1"),
            OsString::from("--manual"),
            OsString::from("item-1"),
            native.clone(),
        ])
        .expect("native manual target must parse without UTF-8 conversion");
        let Command::Plan {
            command: Some(PlanCommand::Revise { manual, .. }),
            ..
        } = cli.command
        else {
            panic!("expected plan revise")
        };
        assert_eq!(manual[1].as_bytes(), native.as_bytes());
    }
}
