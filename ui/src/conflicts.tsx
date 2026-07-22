import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Log, PlanConflictDetail, PlanItem, sourceFileName } from "./model";

function DiagnosticPath({ label, path }: { label: string; path?: string }) {
  return <div className="diagnostic-path"><span>{label}</span><div><strong>{path ? sourceFileName(path) : "—"}</strong>{path && <code title={path}>{path}</code>}</div>{path && <button className="secondary compact" type="button" onClick={() => void navigator.clipboard.writeText(path)}>コピー</button>}</div>;
}

export function PlanConflictCard({ database, planId, item }: { database: string; planId: string; item: PlanItem }) {
  const [detail, setDetail] = useState<PlanConflictDetail>();
  const [loadError, setLoadError] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const load = async () => {
    if (!item.conflict_group_id) return;
    setLoadError(false);
    try { setDetail(await invoke<PlanConflictDetail>("get_plan_conflict_detail", { database, planId, conflictGroupId: item.conflict_group_id })); }
    catch { setLoadError(true); }
  };
  useEffect(() => { void load(); }, [database, planId, item.conflict_group_id]);
  const others = detail?.members.filter(member => member.item_id !== item.id) ?? [];
  const visible = expanded ? others : others.slice(0, 1);
  return <div className="conflict-diagnostic" aria-label="ファイル衝突の詳細">
    <strong>ファイルの衝突</strong>
    <DiagnosticPath label="対象ファイル" path={item.source_path} />
    {detail && <>
      {visible.map((member, index) => <DiagnosticPath key={member.item_id} label={index === 0 ? "衝突相手" : "ほかの相手"} path={member.source_path} />)}
      <DiagnosticPath label="共通の移動先" path={detail.target_path} />
      {others.length > 1 && <button className="secondary compact" type="button" onClick={() => setExpanded(value => !value)}>{expanded ? "相手を1件だけ表示" : `ほか${others.length - 1}件を表示（全${others.length}件）`}</button>}
    </>}
    {!detail && !loadError && <span className="diagnostic-status">衝突相手を読み込んでいます…</span>}
    {loadError && <div className="diagnostic-status error">衝突相手を読み込めませんでした。<button className="secondary compact" type="button" onClick={() => void load()}>再試行</button></div>}
  </div>;
}

export function ExistingTargetConflict({ log }: { log: Log }) {
  return <div className="conflict-diagnostic log-conflict" aria-label="既存ファイルとの衝突">
    <strong>既存ファイルとの衝突</strong>
    <DiagnosticPath label="対象ファイル" path={log.source_path} />
    <DiagnosticPath label="衝突相手" path={log.target_path} />
    <DiagnosticPath label="共通の移動先" path={log.target_path} />
  </div>;
}
