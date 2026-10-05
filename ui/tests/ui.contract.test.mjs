import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  conflictCommandMockContract,
  dialogRuntimeContract,
  jobSnapshotReducerContract,
  losslessClipboardValue,
  reducerStaleGenerationContract,
  renderConflictContracts,
  renderDialogContracts,
  renderLosslessPathContract,
  renderRecoveryContract,
} from "../.test-dist/ui-contract.mjs";

test("workflow reducer ignores stale generations", () => {
  const contract = reducerStaleGenerationContract();
  assert.equal(contract.fixtureExecutionId, "restored-execution");
  assert.equal(contract.exercisedExecutionId, "restored-execution");
  assert.equal(contract.staleExecutionIgnored, true);
  assert.equal(contract.staleFailureIgnored, true);
  assert.equal(contract.staleProgressIgnored, true);
  assert.equal(contract.cancellationSettled, true);
});

test("job snapshots reject wrong, duplicate, and delayed events and restore after reload", () => {
  assert.deepEqual(jobSnapshotReducerContract(), {
    wrongJobIgnored: true,
    staleGenerationIgnored: true,
    duplicateIgnored: true,
    completedPlanId: "plan-1",
    lateTerminalIgnored: true,
    reloadExecutionId: "execution-reload",
    reloadSettled: true,
  });
});

test("confirmation and target edit dialogs retain their ARIA and confirmation markup", () => {
  const { confirm, targetEdit } = renderDialogContracts();

  assert.match(confirm, /aria-labelledby="confirmation-title"/);
  assert.match(confirm, /aria-describedby="confirmation-message"/);
  assert.match(confirm, /<h2 id="confirmation-title">履歴を削除<\/h2>/);
  assert.match(confirm, /<p id="confirmation-message">この操作は取り消せません。<\/p>/);
  assert.match(confirm, /<form>/);
  assert.match(confirm, /type="submit"/);
  assert.match(confirm, /class="danger"/);
  assert.match(confirm, />確認して削除<\/button>/);

  assert.match(targetEdit, /aria-labelledby="target-edit-title"/);
  assert.match(targetEdit, /aria-describedby="target-edit-description"/);
  assert.match(targetEdit, /<h2 id="target-edit-title">移動先を変更<\/h2>/);
  assert.match(targetEdit, /<p id="target-edit-description">元のPlanは変更せず、新しい改訂Planを作成します。<\/p>/);
  assert.match(targetEdit, /<label>新しい移動先<input/);
  assert.match(targetEdit, />改訂Planを作成<\/button>/);
});

test("conflict diagnostics render their accessible labels and protected paths", () => {
  const { planConflict, existingTarget } = renderConflictContracts();

  assert.match(planConflict, /aria-label="ファイル衝突の詳細"/);
  assert.match(planConflict, />ファイルの衝突<\/strong>/);
  assert.match(planConflict, />対象ファイル<\/span>/);
  assert.match(planConflict, />current\.mp3<\/strong>/);
  assert.match(planConflict, /衝突相手を読み込んでいます…/);

  assert.match(existingTarget, /aria-label="既存ファイルとの衝突"/);
  assert.match(existingTarget, />既存ファイルとの衝突<\/strong>/);
  assert.match(existingTarget, />current\.mp3<\/strong>/);
  assert.match(existingTarget, />existing\.mp3<\/strong>/);
  assert.match(existingTarget, />共通の移動先<\/span>/);
});

test("recovery panel exposes the fenced subject, observed paths, and dry-run-first actions", () => {
  const recovery = renderRecoveryContract();
  assert.match(recovery, /aria-labelledby="recovery-title"/);
  assert.match(recovery, /安全な復旧/);
  assert.match(recovery, /content_verified/);
  assert.match(recovery, /operation-recovery-1/);
  assert.match(recovery, /track\.mp3/);
  assert.match(recovery, />復旧Dry-run<\/button>/);
  assert.match(recovery, />照合して復旧を実行<\/button>/);
  assert.match(recovery, /復旧Dry-run成功/);
});

test("native modal behavior handles initial focus, Escape, return focus, failure, and retry", async () => {
  const behavior = await dialogRuntimeContract();
  assert.deepEqual(behavior, {
    modalOpened: 1,
    initialFocus: 1,
    prevented: 1,
    closed: 1,
    scheduled: 1,
    returnedFocus: 1,
    failures: 1,
    successes: 1,
  });
});

test("conflict command boundary exposes deterministic failure and retry", async () => {
  const behavior = await conflictCommandMockContract();
  assert.deepEqual(behavior, { calls: 2, failed: true, recoveredId: "group-1" });
});

test("every statically invoked UI command is registered by the Tauri handler", () => {
  const repository = fileURLToPath(new URL("../..", import.meta.url));
  const uiSource = readdirSync(`${repository}/ui/src`).filter(name => /\.tsx?$/.test(name)).map(name => readFileSync(`${repository}/ui/src/${name}`, "utf8")).join("\n");
  const desktopSource = readFileSync(`${repository}/crates/desktop/src/main.rs`, "utf8");
  const handler = desktopSource.match(/\.invoke_handler\(tauri::generate_handler!\[([\s\S]*?)\]\)/)?.[1];
  assert.ok(handler, "the Tauri handler declaration must remain discoverable");
  const invoked = new Set(
    [...uiSource.matchAll(/invoke(?:<[^>]+>)?\(\s*"([a-z0-9_]+)"/g)].map(match => match[1]),
  );
  const registered = new Set(
    [...handler.matchAll(/\b([a-z][a-z0-9_]*)\s*,?/g)].map(match => match[1]),
  );
  assert.deepEqual(
    [...invoked].filter(command => !registered.has(command)).sort(),
    [],
    "a UI command without a handler always fails at runtime",
  );
});

test("lossless paths disclose lossy display and copy the raw envelope", () => {
  const rendered = renderLosslessPathContract();
  assert.match(rendered, /表示できない文字を含む/);
  assert.match(rendered, /可逆形式でコピー/);
  const envelope = {
    schema_version: 1,
    role: "plan_source",
    display: "C:\\Incoming\\表示�.flac",
    display_lossy: true,
    encoding: "windows_utf16le_v1",
    raw_base64: "QwA6AFwAANg=",
  };
  assert.deepEqual(JSON.parse(losslessClipboardValue(envelope)), envelope);
});
