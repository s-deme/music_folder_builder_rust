use clap::{Parser, Subcommand};
use music_folder_core::usecases::{
    ApplyUseCase, PlanOptions, PlanUseCase, RollbackUseCase, ScanOptions, ScanUseCase,
    VerifyUseCase,
};
use music_folder_infra::{
    lofty_reader::LoftyMetadataReader, sqlite::SqliteScanStore, windows_fs::LocalFileSystem,
};
use std::{path::PathBuf, sync::Arc, time::Instant};

#[derive(Parser)]
#[command(name = "music-folder", about = "Safe music library organizer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Scan {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long)]
        workers: Option<usize>,
    },
    Plan {
        #[arg(long)]
        scan_run_id: String,
        #[arg(long)]
        target: PathBuf,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
    },
    Apply {
        #[arg(long)]
        plan_run_id: String,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        /// Actually mutate files. Without this flag, apply is always a dry-run.
        #[arg(long)]
        execute: bool,
    },
    Verify {
        #[arg(long)]
        execution_run_id: String,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
    },
    Rollback {
        #[arg(long)]
        execution_run_id: String,
        #[arg(long, default_value = "music-folder.db")]
        db: PathBuf,
        #[arg(long)]
        execute: bool,
    },
    /// Measures a cold scan followed by a metadata-cache warm scan and emits JSON.
    Benchmark {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value = "music-folder-benchmark.db")]
        db: PathBuf,
        #[arg(long)]
        workers: Option<usize>,
    },
}
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = Cli::parse();
    match cli.command {
        Command::Scan {
            source,
            db,
            workers,
        } => {
            let store = Arc::new(SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?);
            let mut options = ScanOptions::default();
            if let Some(value) = workers {
                options.workers = value.max(1);
            }
            let usecase = ScanUseCase {
                fs: Arc::new(LocalFileSystem),
                metadata: Arc::new(LoftyMetadataReader),
                store,
            };
            let result = usecase
                .execute(&source, &options)
                .map_err(anyhow::Error::msg)?;
            println!(
                "scan_run_id={} files={} cache_hits={} warnings={}",
                result.scan_id, result.files, result.cache_hits, result.warnings
            );
        }
        Command::Plan {
            scan_run_id,
            target,
            db,
        } => {
            let store = Arc::new(SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?);
            let result = PlanUseCase { store }
                .execute(
                    &scan_run_id,
                    &PlanOptions {
                        target_root: target,
                        batch_size: 250,
                    },
                )
                .map_err(anyhow::Error::msg)?;
            println!(
                "plan_run_id={} items={} conflicts={} risks={}",
                result.plan_id, result.items, result.conflicts, result.risks
            );
        }
        Command::Apply {
            plan_run_id,
            db,
            execute,
        } => {
            let store = Arc::new(SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?);
            let result = ApplyUseCase {
                store,
                files: Arc::new(LocalFileSystem),
            }
            .execute(&plan_run_id, !execute)
            .map_err(anyhow::Error::msg)?;
            println!(
                "execution_run_id={} mode={} success={} skipped={} failed={}",
                result.execution_id,
                if execute { "apply" } else { "dry_run" },
                result.success,
                result.skipped,
                result.failed
            );
        }
        Command::Verify {
            execution_run_id,
            db,
        } => {
            let store = Arc::new(SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?);
            let result = VerifyUseCase {
                store,
                files: Arc::new(LocalFileSystem),
            }
            .execute(&execution_run_id)
            .map_err(anyhow::Error::msg)?;
            println!(
                "verify execution_run_id={} success={} failed={}",
                execution_run_id, result.success, result.failed
            );
        }
        Command::Rollback {
            execution_run_id,
            db,
            execute,
        } => {
            let store = Arc::new(SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?);
            let result = RollbackUseCase {
                store,
                files: Arc::new(LocalFileSystem),
            }
            .execute(&execution_run_id, !execute)
            .map_err(anyhow::Error::msg)?;
            println!(
                "rollback execution_run_id={} mode={} success={} skipped={} failed={}",
                execution_run_id,
                if execute { "rollback" } else { "dry_run" },
                result.success,
                result.skipped,
                result.failed
            );
        }
        Command::Benchmark {
            source,
            db,
            workers,
        } => {
            let mut options = ScanOptions::default();
            if let Some(value) = workers {
                options.workers = value.max(1);
            }
            let run = |options: &ScanOptions| -> Result<_, anyhow::Error> {
                let started = Instant::now();
                ScanUseCase {
                    fs: Arc::new(LocalFileSystem),
                    metadata: Arc::new(LoftyMetadataReader),
                    store: Arc::new(SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?),
                }
                .execute(&source, options)
                .map(|result| (result, started.elapsed().as_millis() as u64))
                .map_err(anyhow::Error::msg)
            };
            let (cold, cold_ms) = run(&options)?;
            let (warm, warm_ms) = run(&options)?;
            let store = SqliteScanStore::open(&db).map_err(anyhow::Error::msg)?;
            let rate = |files: u64, elapsed_ms: u64| {
                if elapsed_ms == 0 {
                    files as f64
                } else {
                    files as f64 * 1000.0 / elapsed_ms as f64
                }
            };
            let phases = |run_id: &str| -> Result<Vec<_>, anyhow::Error> {
                store.list_metrics(run_id).map_err(anyhow::Error::msg)
            };
            let output = serde_json::json!({
                "cold": {"scan_id": cold.scan_id, "files": cold.files, "cache_hits": cold.cache_hits, "elapsed_ms": cold_ms, "items_per_second": rate(cold.files, cold_ms), "phases": phases(&cold.scan_id)?},
                "warm": {"scan_id": warm.scan_id, "files": warm.files, "cache_hits": warm.cache_hits, "cache_hit_rate": if warm.files == 0 { 0.0 } else { warm.cache_hits as f64 / warm.files as f64 }, "elapsed_ms": warm_ms, "items_per_second": rate(warm.files, warm_ms), "phases": phases(&warm.scan_id)?},
                "rss_bytes": current_rss_bytes(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
    };
    Ok(())
}

fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let pages = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|value| value.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0);
        pages * 4096
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}
