import React from "react";
import type { RecoveryItem, RecoveryResult } from "./model";
import { PathValue } from "./paths";

export const recoveryActionLabels: Record<string, string> = {
  resume: "検証済み内容から処理を再開",
  rollback_published: "公開済みtargetを取り消す",
  discard_unpublished_temporary: "未公開の一時ファイルを破棄",
  manual_intervention: "手動確認が必要",
};

export function RecoveryPanel({
  loading,
  items,
  result,
  actionsAllowed,
  onRefresh,
  onDryRun,
  onExecute,
}: {
  loading: boolean;
  items: RecoveryItem[];
  result?: RecoveryResult;
  actionsAllowed: boolean;
  onRefresh: () => void;
  onDryRun: (item: RecoveryItem) => void;
  onExecute: (item: RecoveryItem) => void;
}) {
  return (
    <section className="recovery-panel" aria-labelledby="recovery-title">
      <div className="section-title">
        <div>
          <h2 id="recovery-title">安全な復旧</h2>
          <p>前回終了時に完了しなかったファイル操作を、通常のApply/Rollbackより先に解決します。</p>
        </div>
        <button className="secondary" disabled={loading} onClick={onRefresh}>
          {loading ? "確認中…" : "復旧状態を再確認"}
        </button>
      </div>
      {items.map(item => (
        <article className="recovery-item" key={item.operation_id}>
          <header>
            <strong>#{item.sequence_no} {recoveryActionLabels[item.suggested_action] ?? item.suggested_action}</strong>
            <span className="status-badge status-failed">{item.state}</span>
          </header>
          <div className="path-line"><span>元</span><PathValue path={item.source} copy /></div>
          <div className="path-line"><span>一時</span><PathValue path={item.temporary} copy /></div>
          <div className="path-line"><span>移動先</span><PathValue path={item.target} copy /></div>
          <small>operation <code>{item.operation_id}</code>{item.expected_size !== undefined && ` / 期待サイズ ${item.expected_size.toLocaleString()} bytes`}</small>
          {item.suggested_action === "manual_intervention" ? (
            <p className="error" role="alert">状態を一意に判定できません。既存ファイルは変更せず、診断export後に手動確認してください。</p>
          ) : (
            <div className="actions">
              <button disabled={loading || !actionsAllowed} onClick={() => onDryRun(item)}>復旧Dry-run</button>
              <button className="danger" disabled={loading || !actionsAllowed} onClick={() => onExecute(item)}>照合して復旧を実行</button>
            </div>
          )}
        </article>
      ))}
      {items.length === 0 && !loading && <p className="success">未解決の復旧対象はありません。</p>}
      {result && (
        <output className={result.verify_failed > 0 ? "error" : "success"}>
          {result.dry_run
            ? `復旧Dry-run成功: ${recoveryActionLabels[result.action] ?? result.action}`
            : `復旧完了・Verify成功 ${result.verify_success} / 不一致 ${result.verify_failed}`}
        </output>
      )}
    </section>
  );
}
