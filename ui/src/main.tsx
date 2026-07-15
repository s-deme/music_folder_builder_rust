import React, { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";

type Scan = { scan_id: string; files: number; cache_hits: number; warnings: number };
type ScanStatus = { request_id: string; status: string; scan_id?: string; files: number; cache_hits: number; warnings: number; error?: string };
type Progress = { scan_id: string; phase: string; enumerated: number; processed: number; cache_hits: number; warnings: number; elapsed_ms: number; items_per_second: number; eta_seconds?: number };
type Workflow = { id: string; success: number; skipped: number; failed: number };
type History = { id: string; kind: string; status: string; started_at: number };
type RunDetail = { id: string; kind: string; status: string; parent_id?: string; success: number; skipped: number; failed: number };
type PlanItem = { id: string; ordinal: number; source_path: string; target_path?: string; action: string; risk: string; reason?: string };
type Log = { id: string; execution_id: string; sequence_no: number; source_path: string; target_path?: string; action: string; result: string; error?: string; created_at: number };
type Metric = { phase: string; elapsed_ms: number; item_count: number };
type NamingRules = { artist_dir_template: string; album_dir_template: string; disc_dir_template: string; filename_template: string; duplicate_suffix_template: string; use_source_filename: boolean; use_source_image_filename: boolean };
type CleanupPreview = { plans: number; executions: number; logs: number; blocked: boolean };
const defaultNaming: NamingRules = { artist_dir_template: "{album_artist}", album_dir_template: "{album}", disc_dir_template: "[{disc_no:02d}]", filename_template: "[{track_no:02d}_]{title}{extension}", duplicate_suffix_template: "", use_source_filename: false, use_source_image_filename: false };

const ja = {
  title: "Music Folder Builder", subtitle: "安全な段階型音楽ライブラリ整理", source: "音楽フォルダ", target: "整理先", database: "状態DB",
  scan: "Scan", plan: "Plan", dry: "Dry-run", apply: "Apply", verify: "Verify", rollback: "Rollback dry-run", rollbackApply: "Rollbackを実行", cancel: "Scanを取消",
  workflow: "ワークフロー", history: "実行履歴", logs: "実行ログ", planned: "整理予定", more: "さらに読み込む", refresh: "更新", theme: "テーマ",
  all: "すべて", success: "成功", failed: "失敗", skipped: "スキップ", conflict: "衝突", missing: "メタデータ不足", longPath: "長いパス",
  planSearch: "予定を検索", logSearch: "ログを検索", system: "システム", light: "ライト", dark: "ダーク",
};

function App() {
  const [source, setSource] = useState(() => localStorage.getItem("mfb.source") ?? "");
  const [namingText, setNamingText] = useState(() => localStorage.getItem("mfb.naming") ?? JSON.stringify(defaultNaming, null, 2));
  const [target, setTarget] = useState(() => localStorage.getItem("mfb.target") ?? "");
  const [database, setDatabase] = useState(() => localStorage.getItem("mfb.database") ?? "music-folder.db");
  const [scan, setScan] = useState<Scan>(); const [scanRequest, setScanRequest] = useState<ScanStatus>(); const [progress, setProgress] = useState<Progress>();
  const [plan, setPlan] = useState<Workflow>(); const [execution, setExecution] = useState<Workflow>(); const [executionId, setExecutionId] = useState<string>();
  const [history, setHistory] = useState<History[]>([]); const [items, setItems] = useState<PlanItem[]>([]); const [logs, setLogs] = useState<Log[]>([]); const [metrics, setMetrics] = useState<Metric[]>([]);
  const [query, setQuery] = useState(""); const [risk, setRisk] = useState(""); const [logQuery, setLogQuery] = useState(""); const [logResult, setLogResult] = useState("");
  const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false); const [theme, setTheme] = useState<"system" | "light" | "dark">(() => localStorage.getItem("theme") as "system" | "light" | "dark" || "system");
  const planRef = useRef<HTMLDivElement>(null); const logRef = useRef<HTMLDivElement>(null);
  const planVirtualizer = useVirtualizer({ count: items.length, getScrollElement: () => planRef.current, estimateSize: () => 48, overscan: 8 });
  const logVirtualizer = useVirtualizer({ count: logs.length, getScrollElement: () => logRef.current, estimateSize: () => 48, overscan: 8 });

  useEffect(() => { document.documentElement.dataset.theme = theme; localStorage.setItem("theme", theme); }, [theme]);
  useEffect(() => { localStorage.setItem("mfb.source", source); }, [source]);
  useEffect(() => { localStorage.setItem("mfb.target", target); }, [target]);
  useEffect(() => { localStorage.setItem("mfb.database", database); }, [database]);
  useEffect(() => { localStorage.setItem("mfb.naming", namingText); }, [namingText]);
  useEffect(() => { let offProgress: (() => void) | undefined; let offFinished: (() => void) | undefined;
    void listen<Progress>("scan-progress", event => setProgress(event.payload)).then(off => offProgress = off);
    void listen<ScanStatus>("scan-finished", event => { const value = event.payload; setScanRequest(value); setBusy(false); setProgress(undefined); if (value.status === "completed" && value.scan_id) setScan({ scan_id: value.scan_id, files: value.files, cache_hits: value.cache_hits, warnings: value.warnings }); if (value.error) setError(value.error); }).then(off => offFinished = off);
    return () => { offProgress?.(); offFinished?.(); };
  }, []);
  useEffect(() => {
    if (!scanRequest || scanRequest.status !== "running") return;
    let stopped = false;
    const poll = async () => {
      try {
        const value = await invoke<ScanStatus>("scan_status", { requestId: scanRequest.request_id });
        if (stopped) return;
        setScanRequest(value);
        if (value.status !== "running") {
          setBusy(false);
          setProgress(undefined);
          if (value.status === "completed" && value.scan_id) setScan({ scan_id: value.scan_id, files: value.files, cache_hits: value.cache_hits, warnings: value.warnings });
          if (value.error) setError(value.error);
        }
      } catch (reason) {
        if (!stopped && String(reason) !== "scan_not_found") setError(String(reason));
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 500);
    return () => { stopped = true; window.clearInterval(timer); };
  }, [scanRequest?.request_id, scanRequest?.status]);
  async function run<T>(work: () => Promise<T>, save: (value: T) => void) { setBusy(true); setError(undefined); try { save(await work()); } catch (reason) { setError(String(reason)); } finally { setBusy(false); } }
  async function startScan() { setBusy(true); setError(undefined); setProgress(undefined); setScan(undefined); setPlan(undefined); try { setScanRequest(await invoke("start_scan", { source, database, workers: null })); } catch (reason) { setError(String(reason)); setBusy(false); } }
  async function loadItems(reset = false) { if (!plan) return; const rows = await invoke<PlanItem[]>("list_plan_items", { database, planId: plan.id, cursor: reset ? undefined : items.at(-1)?.ordinal, limit: 200, query: query || null, risk: risk || null }); setItems(reset ? rows : [...items, ...rows]); if (reset) planRef.current?.scrollTo({ top: 0 }); }
  async function loadLogs(reset = false) { if (!executionId) return; const rows = await invoke<Log[]>("list_operation_logs", { database, executionId, cursor: reset ? undefined : logs.at(-1)?.sequence_no, limit: 200, query: logQuery || null, result: logResult || null }); setLogs(reset ? rows : [...logs, ...rows]); if (reset) { logRef.current?.scrollTo({ top: 0 }); setMetrics(await invoke("list_metrics", { database, runId: executionId })); } }
  async function loadHistory(reset = false) { const rows = await invoke<History[]>("list_history", { database, limit: 100, cursor: reset ? null : history.at(-1)?.started_at }); setHistory(reset ? rows : [...history, ...rows]); }
  async function restoreHistory(value: History) { setError(undefined); try { const detail = await invoke<RunDetail>("get_run_detail", { database, kind: value.kind, runId: value.id }); const result = { id: detail.id, success: detail.success, skipped: detail.skipped, failed: detail.failed }; if (detail.kind === "scan") { setScan({ scan_id: detail.id, files: detail.success, cache_hits: 0, warnings: detail.failed }); setScanRequest(undefined); setProgress(undefined); } else if (detail.kind === "plan") { setPlan(result); } else if (detail.kind === "apply") { setExecution(result); setExecutionId(detail.id); } else { setExecution(result); if (detail.parent_id) setExecutionId(detail.parent_id); } setMetrics(await invoke("list_metrics", { database, runId: detail.id })); } catch (reason) { setError(String(reason)); } }
  useEffect(() => { if (plan) void loadItems(true); }, [plan?.id, query, risk]);
  useEffect(() => { if (executionId) void loadLogs(true); }, [executionId, logQuery, logResult]);

  return <main>
    <header><h1>{ja.title}</h1><p>{ja.subtitle}</p><label>{ja.theme}<select value={theme} onChange={event => setTheme(event.target.value as typeof theme)}><option value="system">{ja.system}</option><option value="light">{ja.light}</option><option value="dark">{ja.dark}</option></select></label></header>
    <nav>{[ja.scan, ja.plan, ja.dry, ja.apply, ja.verify, ja.rollback].map((label, index) => <span className={index === 0 ? "active" : ""} key={label}>{index + 1}. {label}</span>)}</nav>
    <section><h2>{ja.workflow}</h2><label>{ja.source}<input value={source} onChange={event => setSource(event.target.value)} /></label><label>{ja.target}<input value={target} onChange={event => setTarget(event.target.value)} /></label><label>{ja.database}<input value={database} onChange={event => setDatabase(event.target.value)} /></label>
      <label>命名設定(JSON)<textarea value={namingText} onChange={event => setNamingText(event.target.value)} /></label><div className="actions"><button disabled={!source || busy} onClick={() => void startScan()}>{ja.scan}</button><button disabled={!scanRequest || scanRequest.status !== "running"} onClick={() => void invoke("cancel_scan", { scanId: scanRequest?.request_id })}>{ja.cancel}</button><button disabled={!scan || !target || busy} onClick={() => run<Workflow>(() => invoke("create_plan", { scanId: scan?.scan_id, target, database, naming: JSON.parse(namingText) as NamingRules }), setPlan)}>{ja.plan}</button><button disabled={!plan || busy} onClick={() => run<Workflow>(() => invoke("apply_plan", { planId: plan?.id, database, execute: false }), value => { setExecution(value); setExecutionId(value.id); })}>{ja.dry}</button><button className="danger" disabled={!plan || busy} onClick={() => plan && confirm(`保存済みPlan ${plan.id} のsnapshotを検証して本実行します。`) && run<Workflow>(() => invoke("apply_plan", { planId: plan.id, database, execute: true }), value => { setExecution(value); setExecutionId(value.id); })}>{ja.apply}</button><button disabled={!executionId || busy} onClick={() => run<Workflow>(() => invoke("verify_execution", { executionId, database }), setExecution)}>{ja.verify}</button><button disabled={!executionId || busy} onClick={() => run<Workflow>(() => invoke("rollback_execution", { executionId, database, execute: false }), setExecution)}>{ja.rollback}</button><button className="danger" disabled={!executionId || busy} onClick={() => executionId && confirm(`実行 ${executionId} を巻き戻します。target の削除を伴う場合があります。`) && run<Workflow>(() => invoke("rollback_execution", { executionId, database, execute: true }), setExecution)}>{ja.rollbackApply}</button></div>
      {scanRequest && <output>Scan状態: {scanRequest.status}</output>}{progress && <output>{progress.phase}: {progress.processed}/{progress.enumerated}件 / {progress.items_per_second.toFixed(1)}件/秒 / ETA {progress.eta_seconds ?? "-"}秒 / cache {progress.cache_hits} / 警告 {progress.warnings}</output>}{scan && <output>Scan: {scan.files}件 / cache {scan.cache_hits} / 警告 {scan.warnings}</output>}{plan && <output>Plan ID {plan.id}: {plan.success}件 / conflict {plan.skipped} / risk {plan.failed}</output>}{execution && <output>実行: success {execution.success} / skip {execution.skipped} / fail {execution.failed}</output>}{error && <p className="error">{error}</p>}
    </section>
    <section><h2>{ja.logs}</h2><input placeholder={ja.logSearch} value={logQuery} onChange={event => setLogQuery(event.target.value)} /><select value={logResult} onChange={event => setLogResult(event.target.value)}><option value="">{ja.all}</option><option value="success">{ja.success}</option><option value="failed">{ja.failed}</option><option value="skipped">{ja.skipped}</option></select>
      <div className="virtual" ref={logRef}><div style={{ height: logVirtualizer.getTotalSize(), position: "relative" }}>{logVirtualizer.getVirtualItems().map(row => { const value = logs[row.index]; return <div className="row" data-index={row.index} ref={logVirtualizer.measureElement} key={value.id} style={{ position: "absolute", top: 0, width: "100%", transform: `translateY(${row.start}px)` }}><b>#{value.sequence_no}</b><span>{value.result}</span><code>{value.action} {value.source_path} → {value.target_path}</code>{value.error && <small>{value.error}</small>}</div>; })}</div></div><button disabled={!executionId} onClick={() => void loadLogs()}>{ja.more}</button>{metrics.map(value => <output key={`${value.phase}-${value.elapsed_ms}`}>{value.phase}: {value.elapsed_ms}ms / {value.item_count}件</output>)}
    </section>
    <section><h2>{ja.planned}</h2><input placeholder={ja.planSearch} value={query} onChange={event => setQuery(event.target.value)} /><select value={risk} onChange={event => setRisk(event.target.value)}><option value="">{ja.all}</option><option value="conflict">{ja.conflict}</option><option value="metadata_missing">{ja.missing}</option><option value="path_too_long">{ja.longPath}</option></select>
      <div className="virtual" ref={planRef}><div style={{ height: planVirtualizer.getTotalSize(), position: "relative" }}>{planVirtualizer.getVirtualItems().map(row => { const value = items[row.index]; return <div className="row" data-index={row.index} ref={planVirtualizer.measureElement} key={value.id} style={{ position: "absolute", top: 0, width: "100%", transform: `translateY(${row.start}px)` }}><b>#{value.ordinal}</b><span className={value.risk === "none" ? "" : "risk"}>{value.risk}</span><code>{value.source_path}</code><code>{value.target_path}</code><button disabled={!plan || busy} onClick={() => { const target = prompt("新しい移動先（改訂Planを作成します）", value.target_path ?? ""); if (target && plan) void run<Workflow>(() => invoke("revise_plan_target", { database, planId: plan.id, planItemId: value.id, target }), setPlan); }}>移動先を指定</button></div>; })}</div></div><button disabled={!plan || busy} onClick={() => void loadItems()}>{ja.more} ({items.length})</button>
    </section>
    <section><h2>{ja.history}</h2><button onClick={() => void loadHistory(true)}>{ja.refresh}</button><div className="history">{history.map(value => <div className="row" key={value.id}><button onClick={() => void restoreHistory(value)}><b>{value.kind}</b><span>{value.status}</span><code>{value.id}</code></button><button className="danger" disabled={value.status === "running"} onClick={() => void invoke<CleanupPreview>("history_cleanup_preview", { database, kind: value.kind, runId: value.id }).then(preview => { if (preview.blocked) { setError("実行中の従属履歴があるため削除できません"); return; } if (confirm(`${value.kind} の履歴を削除します。plan ${preview.plans}件、execution ${preview.executions}件、log ${preview.logs}件。実ファイルは変更しません。`)) return invoke("delete_history", { database, kind: value.kind, runId: value.id }).then(() => loadHistory(true)); })}>削除</button></div>)}</div><button onClick={() => void loadHistory()}>{ja.more}</button></section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<React.StrictMode><App /></React.StrictMode>);
