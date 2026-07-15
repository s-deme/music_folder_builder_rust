import React, { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";
import "./naming.css";

type Scan = { scan_id: string; files: number; cache_hits: number; warnings: number };
type ScanStatus = { request_id: string; status: string; scan_id?: string; files: number; cache_hits: number; warnings: number; error?: string };
type Progress = { scan_id: string; phase: string; enumerated: number; processed: number; cache_hits: number; warnings: number; elapsed_ms: number; items_per_second: number; eta_seconds?: number };
type Workflow = { id: string; success: number; skipped: number; failed: number };
type History = { id: string; kind: string; status: string; started_at: number };
type RunDetail = { id: string; kind: string; status: string; parent_id?: string; success: number; skipped: number; failed: number };
type PlanItem = { id: string; ordinal: number; source_path: string; target_path?: string; action: string; risk: string; reason?: string };
type PlanItemCounts = { moves: number; skips: number; needs_attention: number; conflicts: number; invalid_target: number; metadata_missing: number; path_too_long: number };
type PlanItemPage = { items: PlanItem[]; total: number; filtered_total: number; next_cursor?: number; counts: PlanItemCounts };
type Log = { id: string; execution_id: string; sequence_no: number; source_path: string; target_path?: string; action: string; result: string; error?: string; created_at: number };
type Metric = { phase: string; elapsed_ms: number; item_count: number };
type DuplicateStrategy = "skip" | "sequence" | "template";
type NamingRules = { artist_dir_template: string; album_dir_template: string; disc_dir_template: string; filename_template: string; duplicate_suffix_template: string; use_source_filename: boolean; use_source_image_filename: boolean; duplicate_strategy: DuplicateStrategy };
type NamingField = "artist_dir_template" | "album_dir_template" | "disc_dir_template" | "filename_template" | "duplicate_suffix_template";
type NamingPreview = { relative_path: string; issues: { field: string; code: string; message: string }[] };
type CleanupPreview = { plans: number; executions: number; logs: number; blocked: boolean };
const defaultNaming: NamingRules = { artist_dir_template: "{album_artist}", album_dir_template: "{album}", disc_dir_template: "[{disc_no:02d}]", filename_template: "[{track_no:02d}_]{title}{extension}", duplicate_suffix_template: "_{disc_no:02d}", use_source_filename: false, use_source_image_filename: false, duplicate_strategy: "skip" };
const presets: Record<string, NamingRules> = { standard: defaultNaming, flatDisc: { ...defaultNaming, disc_dir_template: "" }, withYear: { ...defaultNaming, album_dir_template: "[{year} - ]{album}" }, source: { ...defaultNaming, use_source_filename: true, use_source_image_filename: true } };
const tokens = ["{album_artist}", "{artist}", "{album}", "{title}", "{track_no:02d}", "{disc_no:02d}", "{year}", "{source_stem}", "{extension}"];
const emptyPlanCounts: PlanItemCounts = { moves: 0, skips: 0, needs_attention: 0, conflicts: 0, invalid_target: 0, metadata_missing: 0, path_too_long: 0 };
const riskLabels: Record<string, string> = { none: "問題なし", conflict: "衝突", invalid_target: "無効な移動先", metadata_missing: "メタデータ不足", path_too_long: "長いパス" };
const actionLabels: Record<string, string> = { move: "移動", skip: "スキップ" };
function loadNaming(): NamingRules { try { return { ...defaultNaming, ...JSON.parse(localStorage.getItem("mfb.naming") ?? "{}") as Partial<NamingRules> }; } catch { return defaultNaming; } }

function NamingEditor({ naming, setNaming, preset, setPreset, preview }: { naming: NamingRules; setNaming: (value: NamingRules) => void; preset: string; setPreset: (value: string) => void; preview?: NamingPreview }) {
  const [field, setField] = useState<NamingField>("filename_template"); const [token, setToken] = useState(tokens[0]);
  const update = (key: keyof NamingRules, value: string | boolean) => { setPreset("custom"); setNaming({ ...naming, [key]: value } as NamingRules); };
  const fields: [NamingField, string][] = [["artist_dir_template", "アーティストフォルダ"], ["album_dir_template", "アルバムフォルダ"], ["disc_dir_template", "ディスクフォルダ"], ["filename_template", "音楽ファイル名"]];
  return <fieldset><legend>命名設定</legend><label>プリセット<select value={preset} onChange={event => { const value = event.target.value; setPreset(value); if (presets[value]) setNaming({ ...presets[value] }); }}><option value="custom">カスタム</option><option value="standard">標準</option><option value="flatDisc">ディスクフォルダなし</option><option value="withYear">年を含める</option><option value="source">元ファイル名を保持</option></select></label>
    {fields.map(([key, label]) => <label key={key}>{label}<input disabled={key === "filename_template" && naming.use_source_filename} value={naming[key]} onChange={event => update(key, event.target.value)} /></label>)}
    <div className="actions token-insert"><select value={field} onChange={event => setField(event.target.value as NamingField)}>{fields.map(([key, label]) => <option value={key} key={key}>{label}</option>)}<option value="duplicate_suffix_template">同名ファイルの末尾</option></select><select value={token} onChange={event => setToken(event.target.value)}>{tokens.map(value => <option key={value}>{value}</option>)}</select><button type="button" onClick={() => update(field, naming[field] + token)}>項目を挿入</button><button type="button" onClick={() => { setPreset("standard"); setNaming({ ...defaultNaming }); }}>既定値に戻す</button></div>
    <label className="check"><input type="checkbox" checked={naming.use_source_filename} onChange={event => update("use_source_filename", event.target.checked)} />元の音楽ファイル名を使用</label><label className="check"><input type="checkbox" checked={naming.use_source_image_filename} onChange={event => update("use_source_image_filename", event.target.checked)} />元の画像ファイル名を使用</label>
    <label>同名ファイルの処理<select value={naming.duplicate_strategy} onChange={event => update("duplicate_strategy", event.target.value)}><option value="skip">安全のためスキップ</option><option value="sequence">安定した連番を付ける</option><option value="template">カスタム末尾を付ける</option></select></label>{naming.duplicate_strategy === "template" && <label>同名ファイルの末尾<input value={naming.duplicate_suffix_template} onChange={event => update("duplicate_suffix_template", event.target.value)} /></label>}
    <div className="naming-preview"><b>生成プレビュー</b><code>{preview?.relative_path ?? "確認中…"}</code>{preview?.issues.map((issue, index) => <p className="error" key={`${issue.field}-${issue.code}-${index}`}>{issue.message}</p>)}</div>
  </fieldset>;
}

const ja = {
  title: "Music Folder Builder", subtitle: "安全な段階型音楽ライブラリ整理", source: "音楽フォルダ", target: "整理先", database: "状態DB",
  scan: "Scan", plan: "Plan", dry: "Dry-run", apply: "Apply", verify: "Verify", rollback: "Rollback dry-run", rollbackApply: "Rollbackを実行", cancel: "Scanを取消",
  workflow: "ワークフロー", history: "実行履歴", logs: "実行ログ", planned: "整理予定", more: "さらに読み込む", refresh: "更新", theme: "テーマ",
  all: "すべて", success: "成功", failed: "失敗", skipped: "スキップ", conflict: "衝突", missing: "メタデータ不足", longPath: "長いパス",
  planSearch: "予定を検索", logSearch: "ログを検索", system: "システム", light: "ライト", dark: "ダーク",
};

function App() {
  const [source, setSource] = useState(() => localStorage.getItem("mfb.source") ?? "");
  const [naming, setNaming] = useState<NamingRules>(loadNaming); const [namingPreset, setNamingPreset] = useState("custom");
  const [namingPreview, setNamingPreview] = useState<NamingPreview>();
  const [target, setTarget] = useState(() => localStorage.getItem("mfb.target") ?? "");
  const [database, setDatabase] = useState(() => localStorage.getItem("mfb.database") ?? "music-folder.db");
  const [scan, setScan] = useState<Scan>(); const [scanRequest, setScanRequest] = useState<ScanStatus>(); const [progress, setProgress] = useState<Progress>();
  const [plan, setPlan] = useState<Workflow>(); const [execution, setExecution] = useState<Workflow>(); const [executionId, setExecutionId] = useState<string>();
  const [history, setHistory] = useState<History[]>([]); const [items, setItems] = useState<PlanItem[]>([]); const [planTotal, setPlanTotal] = useState(0); const [filteredTotal, setFilteredTotal] = useState(0); const [planCounts, setPlanCounts] = useState<PlanItemCounts>(emptyPlanCounts); const [planCursor, setPlanCursor] = useState<number>(); const [logs, setLogs] = useState<Log[]>([]); const [metrics, setMetrics] = useState<Metric[]>([]);
  const [query, setQuery] = useState(""); const [risk, setRisk] = useState(""); const [logQuery, setLogQuery] = useState(""); const [logResult, setLogResult] = useState("");
  const [error, setError] = useState<string>(); const [busy, setBusy] = useState(false); const [theme, setTheme] = useState<"system" | "light" | "dark">(() => localStorage.getItem("theme") as "system" | "light" | "dark" || "system");
  const planRef = useRef<HTMLDivElement>(null); const logRef = useRef<HTMLDivElement>(null);
  const planVirtualizer = useVirtualizer({ count: items.length, getScrollElement: () => planRef.current, estimateSize: () => 144, overscan: 6 });
  const logVirtualizer = useVirtualizer({ count: logs.length, getScrollElement: () => logRef.current, estimateSize: () => 48, overscan: 8 });

  useEffect(() => { document.documentElement.dataset.theme = theme; localStorage.setItem("theme", theme); }, [theme]);
  useEffect(() => { localStorage.setItem("mfb.source", source); }, [source]);
  useEffect(() => { localStorage.setItem("mfb.target", target); }, [target]);
  useEffect(() => { localStorage.setItem("mfb.database", database); }, [database]);
  useEffect(() => { localStorage.setItem("mfb.naming", JSON.stringify(naming)); const timer = window.setTimeout(() => void invoke<NamingPreview>("preview_naming", { naming }).then(setNamingPreview).catch(reason => setError(String(reason))), 150); return () => window.clearTimeout(timer); }, [naming]);
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
  async function loadItems(reset = false) { if (!plan) return; const page = await invoke<PlanItemPage>("list_plan_items", { database, planId: plan.id, cursor: reset ? undefined : planCursor, limit: 200, query: query || null, risk: risk || null }); setItems(previous => reset ? page.items : [...previous, ...page.items]); setPlanTotal(page.total); setFilteredTotal(page.filtered_total); setPlanCounts(page.counts); setPlanCursor(page.next_cursor); if (reset) planRef.current?.scrollTo({ top: 0 }); }
  async function loadLogs(reset = false) { if (!executionId) return; const rows = await invoke<Log[]>("list_operation_logs", { database, executionId, cursor: reset ? undefined : logs.at(-1)?.sequence_no, limit: 200, query: logQuery || null, result: logResult || null }); setLogs(reset ? rows : [...logs, ...rows]); if (reset) { logRef.current?.scrollTo({ top: 0 }); setMetrics(await invoke("list_metrics", { database, runId: executionId })); } }
  async function loadHistory(reset = false) { const rows = await invoke<History[]>("list_history", { database, limit: 100, cursor: reset ? null : history.at(-1)?.started_at }); setHistory(reset ? rows : [...history, ...rows]); }
  async function restoreHistory(value: History) { setError(undefined); try { const detail = await invoke<RunDetail>("get_run_detail", { database, kind: value.kind, runId: value.id }); const result = { id: detail.id, success: detail.success, skipped: detail.skipped, failed: detail.failed }; if (detail.kind === "scan") { setScan({ scan_id: detail.id, files: detail.success, cache_hits: 0, warnings: detail.failed }); setScanRequest(undefined); setProgress(undefined); } else if (detail.kind === "plan") { setPlan(result); } else if (detail.kind === "apply") { setExecution(result); setExecutionId(detail.id); } else { setExecution(result); if (detail.parent_id) setExecutionId(detail.parent_id); } setMetrics(await invoke("list_metrics", { database, runId: detail.id })); } catch (reason) { setError(String(reason)); } }
  useEffect(() => { if (plan) void loadItems(true); }, [plan?.id, query, risk]);
  useEffect(() => { if (executionId) void loadLogs(true); }, [executionId, logQuery, logResult]);

  return <main>
    <header><h1>{ja.title}</h1><p>{ja.subtitle}</p><label>{ja.theme}<select value={theme} onChange={event => setTheme(event.target.value as typeof theme)}><option value="system">{ja.system}</option><option value="light">{ja.light}</option><option value="dark">{ja.dark}</option></select></label></header>
    <nav>{[ja.scan, ja.plan, ja.dry, ja.apply, ja.verify, ja.rollback].map((label, index) => <span className={index === 0 ? "active" : ""} key={label}>{index + 1}. {label}</span>)}</nav>
    <section><h2>{ja.workflow}</h2><label>{ja.source}<input value={source} onChange={event => setSource(event.target.value)} /></label><label>{ja.target}<input value={target} onChange={event => setTarget(event.target.value)} /></label><label>{ja.database}<input value={database} onChange={event => setDatabase(event.target.value)} /></label>
      <NamingEditor naming={naming} setNaming={setNaming} preset={namingPreset} setPreset={setNamingPreset} preview={namingPreview} /><div className="actions"><button disabled={!source || busy} onClick={() => void startScan()}>{ja.scan}</button><button disabled={!scanRequest || scanRequest.status !== "running"} onClick={() => void invoke("cancel_scan", { scanId: scanRequest?.request_id })}>{ja.cancel}</button><button disabled={!scan || !target || busy || !namingPreview || namingPreview.issues.length > 0} onClick={() => run<Workflow>(() => invoke("create_plan", { scanId: scan?.scan_id, target, database, naming }), setPlan)}>{ja.plan}</button><button disabled={!plan || busy} onClick={() => run<Workflow>(() => invoke("apply_plan", { planId: plan?.id, database, execute: false }), value => { setExecution(value); setExecutionId(value.id); })}>{ja.dry}</button><button className="danger" disabled={!plan || busy} onClick={() => plan && confirm(`保存済みPlan ${plan.id} のsnapshotを検証して本実行します。`) && run<Workflow>(() => invoke("apply_plan", { planId: plan.id, database, execute: true }), value => { setExecution(value); setExecutionId(value.id); })}>{ja.apply}</button><button disabled={!executionId || busy} onClick={() => run<Workflow>(() => invoke("verify_execution", { executionId, database }), setExecution)}>{ja.verify}</button><button disabled={!executionId || busy} onClick={() => run<Workflow>(() => invoke("rollback_execution", { executionId, database, execute: false }), setExecution)}>{ja.rollback}</button><button className="danger" disabled={!executionId || busy} onClick={() => executionId && confirm(`実行 ${executionId} を巻き戻します。target の削除を伴う場合があります。`) && run<Workflow>(() => invoke("rollback_execution", { executionId, database, execute: true }), setExecution)}>{ja.rollbackApply}</button></div>
      {scanRequest && <output>Scan状態: {scanRequest.status}</output>}{progress && <output>{progress.phase}: {progress.processed}/{progress.enumerated}件 / {progress.items_per_second.toFixed(1)}件/秒 / ETA {progress.eta_seconds ?? "-"}秒 / cache {progress.cache_hits} / 警告 {progress.warnings}</output>}{scan && <output>Scan: {scan.files}件 / cache {scan.cache_hits} / 警告 {scan.warnings}</output>}{plan && <output>Plan ID {plan.id}: {plan.success}件 / conflict {plan.skipped} / risk {plan.failed}</output>}{execution && <output>実行: success {execution.success} / skip {execution.skipped} / fail {execution.failed}</output>}{error && <p className="error">{error}</p>}
    </section>
    <section><h2>{ja.logs}</h2><input placeholder={ja.logSearch} value={logQuery} onChange={event => setLogQuery(event.target.value)} /><select value={logResult} onChange={event => setLogResult(event.target.value)}><option value="">{ja.all}</option><option value="success">{ja.success}</option><option value="failed">{ja.failed}</option><option value="skipped">{ja.skipped}</option></select>
      <div className="virtual" ref={logRef}><div style={{ height: logVirtualizer.getTotalSize(), position: "relative" }}>{logVirtualizer.getVirtualItems().map(row => { const value = logs[row.index]; return <div className="row" data-index={row.index} ref={logVirtualizer.measureElement} key={value.id} style={{ position: "absolute", top: 0, width: "100%", transform: `translateY(${row.start}px)` }}><b>#{value.sequence_no}</b><span>{value.result}</span><code>{value.action} {value.source_path} → {value.target_path}</code>{value.error && <small>{value.error}</small>}</div>; })}</div></div><button disabled={!executionId} onClick={() => void loadLogs()}>{ja.more}</button>{metrics.map(value => <output key={`${value.phase}-${value.elapsed_ms}`}>{value.phase}: {value.elapsed_ms}ms / {value.item_count}件</output>)}
    </section>
    <section><div className="section-title"><h2>{ja.planned}</h2>{plan && <strong>{planTotal.toLocaleString()}件</strong>}</div>
      {plan && <div className="plan-summary" aria-label="Plan件数"><div><span>移動</span><b>{planCounts.moves.toLocaleString()}</b></div><div><span>スキップ</span><b>{planCounts.skips.toLocaleString()}</b></div><div className={planCounts.needs_attention > 0 ? "attention" : ""}><span>要確認</span><b>{planCounts.needs_attention.toLocaleString()}</b></div></div>}
      <div className="plan-tools"><input aria-label={ja.planSearch} placeholder={ja.planSearch} value={query} onChange={event => setQuery(event.target.value)} /><select aria-label="リスクで絞り込み" value={risk} onChange={event => setRisk(event.target.value)}><option value="">{ja.all} ({(planCounts.moves + planCounts.skips).toLocaleString()})</option><option value="conflict">{ja.conflict} ({planCounts.conflicts.toLocaleString()})</option><option value="invalid_target">無効な移動先 ({planCounts.invalid_target.toLocaleString()})</option><option value="metadata_missing">{ja.missing} ({planCounts.metadata_missing.toLocaleString()})</option><option value="path_too_long">{ja.longPath} ({planCounts.path_too_long.toLocaleString()})</option></select></div>
      <div className="virtual plan-list" ref={planRef}><div style={{ height: planVirtualizer.getTotalSize(), position: "relative" }}>{planVirtualizer.getVirtualItems().map(row => { const value = items[row.index]; return <article className="plan-item" data-index={row.index} ref={planVirtualizer.measureElement} key={value.id} style={{ position: "absolute", top: 0, width: "100%", transform: `translateY(${row.start}px)` }}><header><b>#{value.ordinal}</b><span className="badge">{actionLabels[value.action] ?? value.action}</span>{value.risk !== "none" && <span className="badge risk">{riskLabels[value.risk] ?? value.risk}</span>}</header><div className="path-line"><span>元</span><code title={value.source_path}>{value.source_path}</code></div><div className="path-line"><span>移動先</span><code title={value.target_path}>{value.target_path ?? "—"}</code></div>{value.reason && <div className="reason"><span>理由</span><span>{value.reason}</span></div>}<footer><button className="secondary" disabled={!plan || busy} onClick={() => { const target = prompt("新しい移動先（改訂Planを作成します）", value.target_path ?? ""); if (target && plan) void run<Workflow>(() => invoke("revise_plan_target", { database, planId: plan.id, planItemId: value.id, target }), setPlan); }}>移動先を変更</button></footer></article>; })}</div></div>
      {plan && <div className="plan-pagination"><span>{items.length.toLocaleString()} / {filteredTotal.toLocaleString()}件を表示{filteredTotal !== planTotal && `（全${planTotal.toLocaleString()}件）`}</span>{planCursor !== undefined && <button disabled={busy} onClick={() => void loadItems()}>{ja.more}（次の200件）</button>}</div>}
    </section>
    <section><h2>{ja.history}</h2><button onClick={() => void loadHistory(true)}>{ja.refresh}</button><div className="history">{history.map(value => <div className="row" key={value.id}><button onClick={() => void restoreHistory(value)}><b>{value.kind}</b><span>{value.status}</span><code>{value.id}</code></button><button className="danger" disabled={value.status === "running"} onClick={() => void invoke<CleanupPreview>("history_cleanup_preview", { database, kind: value.kind, runId: value.id }).then(preview => { if (preview.blocked) { setError("実行中の従属履歴があるため削除できません"); return; } if (confirm(`${value.kind} の履歴を削除します。plan ${preview.plans}件、execution ${preview.executions}件、log ${preview.logs}件。実ファイルは変更しません。`)) return invoke("delete_history", { database, kind: value.kind, runId: value.id }).then(() => loadHistory(true)); })}>削除</button></div>)}</div><button onClick={() => void loadHistory()}>{ja.more}</button></section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<React.StrictMode><App /></React.StrictMode>);
