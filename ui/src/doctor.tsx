import React, { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { FolderSelection } from "./model";
import { formatWorkflowError } from "./model";
import { PathValue } from "./paths";
import { doctorStatus, issueLabels, severityLabels, type AlbumDetail, type AlbumRow, type Artwork, type DoctorHistory, type DoctorPanelProps, type DoctorSnapshot, type DoctorSummary, type FileRow, type IssueDetail, type IssueRow, type Page } from "./doctor-model";
import "./doctor.css";

const time = (seconds: number) => new Intl.DateTimeFormat("ja-JP", { dateStyle: "short", timeStyle: "medium" }).format(new Date(seconds * 1000));
const emptyPage = <T,>(): Page<T> => ({ items: [], total: 0, next_cursor: null });

function Pagination({ cursor, page, size, onChange, loading }: { cursor: number; page: Page<unknown>; size: number; onChange: (cursor: number) => void; loading: boolean }) {
  return <div className="doctor-pagination"><span>{page.total === 0 ? "0件" : `${cursor + 1}–${cursor + page.items.length} / ${page.total.toLocaleString()}件`}</span><div><button className="secondary" disabled={loading || cursor === 0} onClick={() => onChange(Math.max(0, cursor - size))}>前へ</button><button className="secondary" disabled={loading || page.next_cursor === null} onClick={() => onChange(page.next_cursor!)}>次へ</button></div></div>;
}

function FileList({ files, duplicates = false }: { files: FileRow[]; duplicates?: boolean }) {
  return <div className="doctor-files">{files.map((file, index) => {
    const tags = file.metadata;
    const disc = tags?.disc_no || 1;
    return <React.Fragment key={file.id}>{(index === 0 || disc !== (files[index - 1].metadata?.disc_no || 1)) && <h4>Disc {disc}{!tags?.disc_no && "（番号未設定は仮に1）"}</h4>}<article className="doctor-file">
      <strong>{tags?.track_no ? `${tags.track_no}. ` : ""}{tags?.title || "Title未設定"}</strong><PathValue path={file.path} copy />
      <dl><dt>Artist</dt><dd>{tags?.artist || "未設定"}</dd><dt>Album / Album Artist</dt><dd>{tags?.album || "未設定"} / {tags?.album_artist || "未設定"}</dd><dt>Year / Genre</dt><dd>{tags?.year || "未設定"} / {tags?.genre || "未設定"}</dd><dt>形式 / サイズ</dt><dd>{file.format.toUpperCase()} / {file.size_bytes.toLocaleString()} bytes</dd><dt>更新日時</dt><dd>{time(Number(BigInt(file.mtime_ns) / BigInt(1_000_000_000)))}</dd><dt>埋め込み画像</dt><dd>{tags?.has_artwork === true ? "あり" : tags?.has_artwork === false ? "なし" : "未調査"}</dd>
      {duplicates && <><dt>SHA-256</dt><dd><code>{file.sha256 || "取得不可"}</code></dd><dt>ファイル実体</dt><dd><code>{file.identity || "不明"}</code></dd></>}</dl>
    </article></React.Fragment>;
  })}</div>;
}

function IssueButton({ issue, selected, onClick }: { issue: IssueRow; selected?: boolean; onClick: () => void }) {
  return <button className={`doctor-issue${selected ? " selected" : ""}`} aria-pressed={selected ?? false} onClick={onClick}><span className={`doctor-severity ${issue.severity}`}>{severityLabels[issue.severity]}</span><strong>{issueLabels[issue.code] ?? issue.code}</strong><small>{issue.file_count}ファイル · {issue.code}</small></button>;
}

export function DoctorPanel({ ready, visible, organizeBusy, onBusy, onOrganize }: DoctorPanelProps) {
  const [folder, setFolder] = useState<FolderSelection>();
  const [job, setJob] = useState<DoctorSnapshot>();
  const [starting, setStarting] = useState(false);
  const [cancelPending, setCancelPending] = useState(false);
  const [picking, setPicking] = useState(false);
  const [history, setHistory] = useState<Page<DoctorHistory>>(emptyPage);
  const [historyCursor, setHistoryCursor] = useState(0);
  const [historyReload, setHistoryReload] = useState(0);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [runId, setRunId] = useState<string>();
  const [summary, setSummary] = useState<DoctorSummary>();
  const [viewReload, setViewReload] = useState(0);
  const [viewLoading, setViewLoading] = useState(false);
  const [tab, setTab] = useState<"issues" | "duplicates" | "albums">("issues");
  const [severity, setSeverity] = useState("");
  const [code, setCode] = useState("");
  const [category, setCategory] = useState("");
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const [issues, setIssues] = useState<Page<IssueRow>>(emptyPage);
  const [albums, setAlbums] = useState<Page<AlbumRow>>(emptyPage);
  const [listLoading, setListLoading] = useState(false);
  const [issueOrdinal, setIssueOrdinal] = useState<number>();
  const [issue, setIssue] = useState<IssueDetail>();
  const [albumId, setAlbumId] = useState<string>();
  const [album, setAlbum] = useState<AlbumDetail>();
  const [fileCursor, setFileCursor] = useState(0);
  const [albumIssueCursor, setAlbumIssueCursor] = useState(0);
  const [detailLoading, setDetailLoading] = useState(false);
  const [images, setImages] = useState<Record<string, Artwork>>({});
  const [error, setError] = useState<string>();
  const activeJob = useRef<string>();
  const terminalJob = useRef<string>();
  const selectionEpoch = useRef(0);
  const selectedRun = useRef(runId);
  selectedRun.current = runId;
  const running = job?.status === "running";
  const busy = starting || running;
  const fail = (reason: unknown) => setError(formatWorkflowError(reason));

  useEffect(() => { onBusy(busy); }, [busy, onBusy]);

  function selectRun(id: string) {
    selectionEpoch.current += 1;
    setViewReload(n => n + 1); setRunId(id); setSummary(undefined); setCursor(0); setIssueOrdinal(undefined); setIssue(undefined); setAlbumId(undefined); setAlbum(undefined); setImages({}); setError(undefined);
  }
  function acceptSnapshot(snapshot: DoctorSnapshot | null) {
    if (!snapshot || (activeJob.current && snapshot.request_id !== activeJob.current)) return;
    activeJob.current = snapshot.request_id;
    setJob(previous => previous?.request_id === snapshot.request_id && previous.status !== "running" && snapshot.status === "running" ? previous : snapshot);
    if (snapshot.status !== "running" && terminalJob.current !== snapshot.request_id) {
      terminalJob.current = snapshot.request_id; setCancelPending(false); setHistoryCursor(0); setHistoryReload(n => n + 1);
      if (snapshot.doctor_run_id) selectRun(snapshot.doctor_run_id);
      if (snapshot.error) setError(snapshot.error);
    }
  }
  useEffect(() => {
    if (!ready) return;
    let stopped = false;
    const update = (snapshot: DoctorSnapshot) => { if (!stopped) acceptSnapshot(snapshot); };
    const listeners = [listen<DoctorSnapshot>("doctor-progress", e => update(e.payload)), listen<DoctorSnapshot>("doctor-finished", e => update(e.payload))];
    const poll = () => invoke<DoctorSnapshot | null>("doctor_status", { requestId: activeJob.current ?? null }).then(s => { if (!stopped) acceptSnapshot(s); }).catch(e => { if (!stopped) fail(e); });
    void poll(); const timer = window.setInterval(() => void poll(), 1000);
    return () => { stopped = true; window.clearInterval(timer); for (const listener of listeners) void listener.then(unlisten => unlisten()).catch(() => undefined); };
  }, [ready]);

  useEffect(() => {
    if (!ready || !visible) return;
    let stopped = false; setHistoryLoading(true);
    void invoke<Page<DoctorHistory>>("doctor_history", { cursor: historyCursor }).then(p => {
      if (stopped) return; setHistory(p);
      if (!selectedRun.current) { const latest = p.items.find(h => h.run.status.toLowerCase() !== "running"); if (latest) selectRun(latest.run.doctor_run_id); }
    }).catch(e => { if (!stopped) fail(e); }).finally(() => { if (!stopped) setHistoryLoading(false); });
    return () => { stopped = true; };
  }, [ready, visible, historyCursor, historyReload]);

  useEffect(() => {
    if (!runId) return;
    let stopped = false; setViewLoading(true);
    void invoke<DoctorSummary>("open_doctor_view", { runId }).then(s => { if (!stopped) setSummary(s); }).catch(e => { if (!stopped) fail(e); }).finally(() => { if (!stopped) setViewLoading(false); });
    return () => { stopped = true; void invoke("close_doctor_view", { runId }).catch(() => undefined); };
  }, [runId, viewReload]);

  useEffect(() => {
    if (!summary || !runId || !visible) return;
    let stopped = false; setListLoading(true); setIssues(emptyPage()); setAlbums(emptyPage());
    const request = tab === "albums"
      ? invoke<Page<AlbumRow>>("doctor_album_page", { runId, query, cursor }).then(p => { if (!stopped) setAlbums(p); })
      : invoke<Page<IssueRow>>("doctor_issue_page", { runId, cursor, severity: severity || null, code: code || null, category: tab === "duplicates" ? "duplicates" : category || null }).then(p => { if (!stopped) setIssues(p); });
    void request.catch(e => { if (!stopped) fail(e); }).finally(() => { if (!stopped) setListLoading(false); });
    return () => { stopped = true; };
  }, [summary, runId, visible, tab, severity, code, category, query, cursor]);

  useEffect(() => {
    if (!summary || !runId || (issueOrdinal === undefined && !albumId)) return;
    let stopped = false; setDetailLoading(true);
    const request = issueOrdinal !== undefined
      ? invoke<IssueDetail>("doctor_issue_detail", { runId, ordinal: issueOrdinal, cursor: fileCursor }).then(p => { if (!stopped) setIssue(p); })
      : invoke<AlbumDetail>("doctor_album_detail", { runId, albumId, cursor: fileCursor, issueCursor: albumIssueCursor }).then(p => { if (!stopped) setAlbum(p); });
    void request.catch(e => { if (!stopped) fail(e); }).finally(() => { if (!stopped) setDetailLoading(false); });
    return () => { stopped = true; };
  }, [summary, runId, issueOrdinal, albumId, fileCursor, albumIssueCursor]);

  useEffect(() => {
    if (!visible || !runId || tab !== "albums" || listLoading || !summary) { setImages({}); return; }
    let stopped = false; let next = 0; setImages({});
    const ids = albums.items.filter(a => !a.unclassified).map(a => a.id);
    const worker = async () => { while (!stopped && next < ids.length) {
      const id = ids[next++];
      try { const image = await invoke<Artwork>("doctor_artwork", { runId, albumId: id }); if (!stopped) setImages(previous => ({ ...previous, [id]: image })); }
      catch { if (!stopped) setImages(previous => ({ ...previous, [id]: { data_url: null, origin: null, source: null, note: "画像を取得できません" } })); }
    } };
    void worker(); void worker();
    return () => { stopped = true; };
  }, [visible, runId, tab, albums, listLoading, summary]);

  async function start() {
    if (!folder) return; setStarting(true); setError(undefined); setCancelPending(false); terminalJob.current = undefined; activeJob.current = undefined;
    try { const snapshot = await invoke<DoctorSnapshot>("start_doctor", { sourceSelectionId: folder.selection_id }); activeJob.current = snapshot.request_id; acceptSnapshot(snapshot); }
    catch (e) { fail(e); } finally { setStarting(false); }
  }
  async function pick() {
    setPicking(true); setError(undefined);
    try { const selected = await invoke<FolderSelection | null>("pick_folder", { purpose: "source", currentSelectionId: folder?.selection_id ?? null }); if (selected) setFolder(selected); }
    catch (e) { fail(e); } finally { setPicking(false); }
  }
  function chooseIssue(ordinal: number) { setIssueOrdinal(ordinal); setIssue(undefined); setAlbumId(undefined); setAlbum(undefined); setFileCursor(0); setAlbumIssueCursor(0); }
  function chooseAlbum(id: string) { setAlbumId(id); setAlbum(undefined); setIssueOrdinal(undefined); setIssue(undefined); setFileCursor(0); setAlbumIssueCursor(0); }
  function chooseTab(value: typeof tab) { setTab(value); setCursor(0); setCode(""); setCategory(""); setIssueOrdinal(undefined); setAlbumId(undefined); setIssue(undefined); setAlbum(undefined); }

  return <div className="doctor-panel" hidden={!visible}>
    <section aria-labelledby="doctor-title"><div className="section-title"><div><h2 id="doctor-title">ライブラリ診断</h2><p>タグ・完全重複・アルバム整合性を確認します。</p></div><span className="doctor-read-only">音楽ファイルは読み取り専用</span></div>
      <div className="folder-field"><label htmlFor="doctor-source">診断する音楽フォルダ</label><div className="folder-picker"><input id="doctor-source" readOnly value={folder?.display ?? ""} placeholder="フォルダを選択してください" /><button className="secondary" disabled={!ready || busy || picking || organizeBusy} onClick={() => void pick()}>{picking ? "選択中…" : "参照…"}</button></div></div>
      <div className="actions"><button disabled={!ready || !folder || busy || organizeBusy || picking} onClick={() => void start()}>診断を開始</button><button className="secondary" disabled={!running || cancelPending} onClick={() => { setCancelPending(true); void invoke("cancel_doctor", { requestId: job!.request_id }).catch(e => { setCancelPending(false); fail(e); }); }}>{cancelPending ? "取消を待っています…" : "診断を取消"}</button>{organizeBusy && <span role="status">整理の処理が終わるまで診断を開始できません。</span>}</div>
      {job && <div className="doctor-progress" role="status" aria-live="polite"><strong>{running ? cancelPending ? "取消処理中" : "診断中" : doctorStatus(job.status)}</strong>{job.progress && <span>{job.progress.phase === "doctor_validate" ? "ファイルを再確認" : "スキャン"} · {job.progress.processed.toLocaleString()}件処理 · キャッシュ {job.progress.cache_hits.toLocaleString()}件</span>}{running && <p>ファイル1件の読取中は取消の完了まで時間がかかることがあります。</p>}</div>}
      {error && <p className="error" role="alert">{error}</p>}
    </section>

    <section aria-labelledby="doctor-history-title"><div className="section-title"><h2 id="doctor-history-title">診断履歴</h2><button className="secondary" disabled={historyLoading} onClick={() => setHistoryReload(n => n + 1)}>更新</button></div>
      {historyLoading && <p role="status">履歴を読み込み中…</p>}
      {!historyLoading && history.items.length === 0 && <p className="empty-state">診断履歴はありません。音楽フォルダを選んで診断を開始してください。</p>}
      <div className="doctor-history">{history.items.map(h => <button key={h.run.doctor_run_id} className={`doctor-history-entry${runId === h.run.doctor_run_id ? " selected" : ""}`} aria-pressed={runId === h.run.doctor_run_id} onClick={() => selectRun(h.run.doctor_run_id)}><time>{time(h.started_at)}</time><strong>{h.source.display}</strong><span>{h.run.status.toLowerCase() === "running" && job?.doctor_run_id !== h.run.doctor_run_id ? "未完了（中断の可能性）" : doctorStatus(h.run.status)}</span><small>{h.run.files.toLocaleString()}ファイル · {h.run.issue_count.toLocaleString()} Issue</small></button>)}</div>
      <Pagination cursor={historyCursor} page={history} size={50} onChange={setHistoryCursor} loading={historyLoading} />
    </section>

    {viewLoading && <section><p role="status">診断結果を読み込み中…</p></section>}
    {summary && runId && <section aria-labelledby="doctor-results-title"><div className="section-title"><div><h2 id="doctor-results-title">診断結果 · {doctorStatus(summary.history.run.status)}</h2><PathValue path={summary.history.source} copy /><small className="doctor-run-id">診断ID: {runId} · 規則 v{summary.history.run.rule_version}</small></div><button className="secondary" disabled={busy || organizeBusy} onClick={() => {
      const epoch = selectionEpoch.current;
      void invoke<FolderSelection>("doctor_source_selection", { runId }).then(selection => { if (epoch === selectionEpoch.current) return onOrganize(selection); }).catch(fail);
    }}>このフォルダを整理</button></div>
      <div className="doctor-summary">{["critical", "warning", "info"].map(s => <div key={s}><span>{severityLabels[s]}</span><strong>{summary.severities[s]?.toLocaleString() ?? 0}</strong></div>)}<div><span>ファイル / キャッシュ</span><strong>{summary.history.run.files.toLocaleString()} / {summary.history.run.cache_hits.toLocaleString()}</strong></div><div><span>失敗観測件数</span><strong>{summary.history.run.failures.toLocaleString()}</strong></div></div>
      {summary.history.run.status.toLowerCase() !== "completed" && <p className="safety-warning">{summary.history.run.status.toLowerCase() === "running" ? "終了が記録されていません。結果は未完了です。" : "部分的な結果です。Issueが0件でも全ファイルを確認できたとは限りません。"}</p>}
      {summary.history.run.error && <p className="error">{summary.history.run.error}</p>}
      <div className="doctor-tabs" role="tablist" aria-label="診断結果の種類">{([["issues", "問題"], ["duplicates", "重複"], ["albums", "アルバム"]] as const).map(([value, label]) => <button key={value} role="tab" aria-selected={tab === value} aria-controls="doctor-result-panel" onClick={() => chooseTab(value)}>{label}</button>)}</div>
      <div id="doctor-result-panel" role="tabpanel" aria-label={tab === "albums" ? "アルバム一覧" : tab === "duplicates" ? "重複一覧" : "問題一覧"}>
      {tab === "albums" ? <label>アルバム名・Artistで検索<input value={query} maxLength={128} onChange={e => { setQuery(e.target.value); setCursor(0); setAlbumId(undefined); setAlbum(undefined); }} /></label> : <div className="doctor-filters"><label>重要度<select value={severity} onChange={e => { setSeverity(e.target.value); setCursor(0); }}><option value="">すべて</option>{Object.entries(severityLabels).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label><label>Issue種類<select value={code} onChange={e => { setCode(e.target.value); setCursor(0); }}><option value="">すべて</option>{Object.entries(issueLabels).filter(([value]) => tab !== "duplicates" || ["exact_duplicate", "same_file_paths"].includes(value)).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>{tab === "issues" && <label>カテゴリ<select value={category} onChange={e => { setCategory(e.target.value); setCursor(0); }}><option value="">すべて</option>{[["read", "読取"], ["tags", "タグ欠損"], ["variants", "表記揺れ"], ["duplicates", "重複"], ["albums", "アルバム整合性"]].map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>}</div>}
      {tab === "duplicates" && <p>バイト単位の一致を示す候補です。同じ実体への別パスは区別して表示します。</p>}
      {tab === "albums" && <p className="doctor-note">画像は閲覧時に読み取ります。フォルダ画像が表示されても、埋め込み画像の欠損Issueは変わりません。</p>}
      {listLoading ? <p role="status">一覧を読み込み中…</p> : <div className="doctor-result-layout"><div>
        {tab === "albums" ? <div className="doctor-albums">{albums.items.map(a => <button key={a.id} className={`doctor-album${albumId === a.id ? " selected" : ""}`} aria-pressed={albumId === a.id} onClick={() => chooseAlbum(a.id)}>{images[a.id]?.data_url ? <img src={images[a.id].data_url!} alt={`${a.title}のジャケット`} width={160} height={160} /> : <span className="doctor-cover-placeholder">{a.unclassified ? "未分類" : images[a.id]?.note ?? "画像を読込中…"}</span>}<strong>{a.title}</strong><span>{a.artist || "Artist不明"}</span><small>{a.tracks}曲 · {a.issue_count} Issue</small>{images[a.id]?.origin && <small>{images[a.id].origin === "embedded" ? "埋め込み画像" : "フォルダ画像"}</small>}</button>)}</div> : <div className="doctor-issues">{issues.items.map(i => <IssueButton key={i.ordinal} issue={i} selected={issueOrdinal === i.ordinal} onClick={() => chooseIssue(i.ordinal)} />)}</div>}
        {(tab === "albums" ? albums : issues).items.length === 0 && <p className="empty-state">{tab === "albums" ? "該当するアルバムはありません。" : "該当するIssueはありません。"}</p>}
      </div><aside className="doctor-detail" aria-label="診断の詳細">
        {detailLoading ? <p role="status">詳細を読み込み中…</p> : issue && issueOrdinal !== undefined ? <><h3>{issueLabels[issue.issue.code] ?? issue.issue.code}</h3><span className={`doctor-severity ${issue.issue.severity}`}>{severityLabels[issue.issue.severity]}</span><h4>検出の根拠</h4><ul>{issue.evidence.map((e, i) => <li key={i}>{e}</li>)}</ul>{issue.comparison && <p>比較値: <code>{issue.comparison}</code></p>}<small>規則 v{issue.rule_version}</small><FileList files={issue.files.items} duplicates={issue.issue.category === "duplicates"} /><Pagination cursor={fileCursor} page={issue.files} size={100} onChange={setFileCursor} loading={detailLoading} /></> : album && albumId ? <><h3>{album.album.title}</h3><p>{album.album.artist}</p><PathValue path={album.album.folder} copy />{images[albumId] && <><p>{images[albumId].note}</p>{images[albumId].source && <PathValue path={images[albumId].source!} />}</>}<h4>関連Issue</h4>{album.issues.items.map(i => <IssueButton key={i.ordinal} issue={i} onClick={() => chooseIssue(i.ordinal)} />)}{album.issues.total === 0 && <p>関連Issueはありません。</p>}<Pagination cursor={albumIssueCursor} page={album.issues} size={100} onChange={setAlbumIssueCursor} loading={detailLoading} /><FileList files={album.files.items} /><Pagination cursor={fileCursor} page={album.files} size={100} onChange={setFileCursor} loading={detailLoading} /></> : <p className="empty-state">一覧から選ぶと、根拠とファイル情報を表示します。</p>}
      </aside></div>}
      <Pagination cursor={cursor} page={tab === "albums" ? albums : issues} size={tab === "albums" ? 48 : 100} onChange={setCursor} loading={listLoading} />
      </div>
    </section>}
  </div>;
}
