import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";

const installedGlobals = [
  "window",
  "document",
  "navigator",
  "localStorage",
  "HTMLElement",
  "HTMLDialogElement",
  "HTMLInputElement",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "Node",
  "MutationObserver",
  "getComputedStyle",
  "requestAnimationFrame",
  "cancelAnimationFrame",
  "IS_REACT_ACT_ENVIRONMENT",
];

function installDom() {
  const previous = new Map(
    installedGlobals.map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]),
  );
  const dom = new JSDOM("<!doctype html><html lang=\"ja\"><body></body></html>", {
    url: "http://localhost/",
    pretendToBeVisual: true,
  });
  const values = {
    window: dom.window,
    document: dom.window.document,
    navigator: dom.window.navigator,
    localStorage: dom.window.localStorage,
    HTMLElement: dom.window.HTMLElement,
    HTMLDialogElement: dom.window.HTMLDialogElement,
    HTMLInputElement: dom.window.HTMLInputElement,
    Event: dom.window.Event,
    MouseEvent: dom.window.MouseEvent,
    KeyboardEvent: dom.window.KeyboardEvent,
    Node: dom.window.Node,
    MutationObserver: dom.window.MutationObserver,
    getComputedStyle: dom.window.getComputedStyle.bind(dom.window),
    requestAnimationFrame: dom.window.requestAnimationFrame.bind(dom.window),
    cancelAnimationFrame: dom.window.cancelAnimationFrame.bind(dom.window),
    IS_REACT_ACT_ENVIRONMENT: true,
  };
  for (const [name, value] of Object.entries(values)) {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  }

  Object.defineProperty(dom.window.navigator, "clipboard", {
    configurable: true,
    value: { writeText: async () => undefined },
  });
  dom.window.HTMLElement.prototype.scrollTo = () => undefined;
  dom.window.HTMLElement.prototype.attachEvent = () => undefined;
  dom.window.HTMLElement.prototype.detachEvent = () => undefined;
  dom.window.HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
  dom.window.HTMLDialogElement.prototype.close = function close() {
    if (!this.hasAttribute("open")) return;
    this.removeAttribute("open");
    this.dispatchEvent(new dom.window.Event("close"));
  };

  return {
    dom,
    restore() {
      dom.window.close();
      for (const name of installedGlobals) {
        const descriptor = previous.get(name);
        if (descriptor) Object.defineProperty(globalThis, name, descriptor);
        else Reflect.deleteProperty(globalThis, name);
      }
    },
  };
}

// ReactDOM performs input-event feature detection at import time. Install a DOM
// for that import so controlled inputs use the same event path as the WebView.
const bootstrapEnvironment = installDom();
const [reactModule, reactDomModule, tauriMockModule, contracts] = await Promise.all([
  import("react"),
  import("react-dom/client"),
  import("@tauri-apps/api/mocks"),
  import("../.test-dist/ui-contract.mjs"),
]);
const React = reactModule.default;
const { act, useState } = reactModule;
const { createRoot } = reactDomModule;
const { clearMocks, mockIPC } = tauriMockModule;
const { App, ConfirmDialog, ExistingTargetConflict, PlanConflictCard } = contracts;
bootstrapEnvironment.restore();

async function mount(element) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(element);
  });
  return {
    container,
    async unmount() {
      await act(async () => root.unmount());
      container.remove();
    },
  };
}

async function waitFor(read, description, timeout = 2_500) {
  const started = Date.now();
  while (Date.now() - started < timeout) {
    let value;
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 15));
      value = read();
    });
    if (value) return value;
  }
  throw new Error(`timed out waiting for ${description}`);
}

function buttonByText(scope, text) {
  return [...scope.querySelectorAll("button")].find(button => button.textContent?.trim() === text);
}

function setControlledInput(input, value) {
  const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")?.set;
  setter?.call(input, value);
  input.dispatchEvent(new window.Event("input", { bubbles: true }));
  input.dispatchEvent(new window.Event("change", { bubbles: true }));
}

function workflowCapabilities({ generation = 0, planId, executionId, recoveryCount = 0 } = {}) {
  const base = { allowed: false, protection_reasons: ["workflow_subject_missing"] };
  const plan = planId
    ? { allowed: recoveryCount === 0, protection_reasons: recoveryCount ? ["recovery_required"] : [], confirmation_subject: planId }
    : base;
  const execution = executionId
    ? { allowed: recoveryCount === 0, protection_reasons: recoveryCount ? ["recovery_required"] : [], confirmation_subject: executionId }
    : base;
  return {
    workflow_generation: generation,
    scan_id: planId ? "scan-test" : undefined,
    plan_id: planId,
    execution_id: executionId,
    recovery_count: recoveryCount,
    apply_dry_run: planId ? { allowed: plan.allowed, protection_reasons: plan.protection_reasons } : base,
    apply_execute: plan,
    verify: executionId ? { allowed: execution.allowed, protection_reasons: execution.protection_reasons } : base,
    rollback_dry_run: executionId ? { allowed: execution.allowed, protection_reasons: execution.protection_reasons } : base,
    rollback_execute: execution,
    recovery_execute: { allowed: recoveryCount > 0, protection_reasons: recoveryCount ? [] : ["recovery_not_required"] },
  };
}

function defaultIpc(command) {
  switch (command) {
    case "desktop_backend_ready": return true;
    case "preview_naming": return { relative_path: "Artist\\Album\\track.flac", issues: [] };
    case "list_active_jobs": return [];
    case "get_job_snapshot": return null;
    case "get_workflow_capabilities": return workflowCapabilities();
    case "list_recovery_items": return [];
    case "list_history": return [];
    case "list_attempt_evidence": return { preflights: [], journal: [], logs: [] };
    case "list_plan_items": return {
      items: [],
      total: 0,
      filtered_total: 0,
      next_cursor: null,
      counts: {
        moves: 0,
        skips: 0,
        needs_attention: 0,
        conflicts: 0,
        invalid_target: 0,
        metadata_missing: 0,
        path_too_long: 0,
      },
    };
    case "list_operation_logs": return [];
    case "list_metrics": return [];
    default: throw new Error(`unexpected IPC command: ${command}`);
  }
}

test("mounted confirmation dialog supports focus, retry, Enter submission, and Escape cancellation", async () => {
  const environment = installDom();
  let attempts = 0;
  let closed = 0;
  const request = {
    title: "本実行しますか？",
    message: "保存済みPlanだけを実行します。",
    confirmLabel: "Applyを実行",
    danger: true,
    onConfirm: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("事前照合に失敗しました");
    },
  };
  function Harness() {
    const [active, setActive] = useState();
    return React.createElement(
      React.Fragment,
      null,
      React.createElement("button", { id: "dialog-opener", onClick: () => setActive(request) }, "確認を開く"),
      React.createElement(ConfirmDialog, {
        request: active,
        onClose: () => {
          closed += 1;
          setActive(undefined);
        },
      }),
    );
  }

  const mounted = await mount(React.createElement(Harness));
  try {
    const opener = mounted.container.querySelector("#dialog-opener");
    opener.focus();
    await act(async () => opener.click());
    const dialog = await waitFor(
      () => mounted.container.querySelector("dialog[open]"),
      "the confirmation dialog to open",
    );
    assert.equal(dialog.getAttribute("aria-labelledby"), "confirmation-title");
    assert.equal(dialog.getAttribute("aria-describedby"), "confirmation-message");
    assert.equal(document.activeElement?.textContent, "キャンセル");

    const submit = buttonByText(dialog, "Applyを実行");
    await act(async () => submit.click());
    const alert = await waitFor(() => dialog.querySelector("[role=alert]"), "the action failure");
    assert.match(alert.textContent, /事前照合に失敗しました/);
    assert.equal(dialog.hasAttribute("open"), true, "failure keeps the modal open for retry");

    await act(async () => buttonByText(dialog, "Applyを実行").click());
    await waitFor(() => !dialog.hasAttribute("open"), "the successful retry to close the dialog");
    await waitFor(() => document.activeElement === opener, "focus to return to the opener");
    assert.equal(attempts, 2);

    await act(async () => opener.click());
    const reopened = await waitFor(
      () => mounted.container.querySelector("dialog[open]"),
      "the dialog to reopen",
    );
    const cancelEvent = new window.Event("cancel", { cancelable: true });
    await act(async () => reopened.dispatchEvent(cancelEvent));
    await waitFor(() => !reopened.hasAttribute("open"), "Escape cancellation to close the dialog");
    await waitFor(() => document.activeElement === opener, "focus to return after Escape");
    assert.equal(cancelEvent.defaultPrevented, true);
    assert.equal(closed, 2);
  } finally {
    await mounted.unmount();
    environment.restore();
  }
});

test("mounted conflict diagnostics cover unloaded, retry, singleton, long, and existing-target cases", async () => {
  const environment = installDom();
  let rejectInitial;
  let retryCalls = 0;
  const longPath = `C:\\Music\\${"とても深いフォルダ\\".repeat(24)}track.flac`;
  mockIPC((command, payload) => {
    if (command !== "get_plan_conflict_detail") return defaultIpc(command);
    if (payload.conflictGroupId === "singleton") {
      return {
        id: "singleton",
        kind: "same_target",
        target_path: "C:\\Music\\single.flac",
        members: [{ item_id: "single", ordinal: 1, source_path: "C:\\Incoming\\single.flac" }],
        candidates: [],
      };
    }
    retryCalls += 1;
    if (retryCalls === 1) {
      return new Promise((_resolve, reject) => { rejectInitial = reject; });
    }
    return {
      id: "retry",
      kind: "same_target",
      target_path: longPath,
      members: [
        { item_id: "current", ordinal: 2, source_path: "C:\\Incoming\\current.flac" },
        { item_id: "other-1", ordinal: 3, source_path: "C:\\Incoming\\other-1.flac" },
        { item_id: "other-2", ordinal: 4, source_path: "C:\\Incoming\\other-2.flac" },
      ],
      candidates: [],
    };
  }, { shouldMockEvents: true });

  const retryItem = {
    id: "current",
    conflict_group_id: "retry",
    conflict_member_count: 3,
    ordinal: 2,
    source_path: "C:\\Incoming\\current.flac",
    target_path: longPath,
    action: "skip",
    risk: "conflict",
    reason: "target_conflict",
  };
  const singletonItem = {
    id: "single",
    conflict_group_id: "singleton",
    conflict_member_count: 1,
    ordinal: 1,
    source_path: "C:\\Incoming\\single.flac",
    target_path: "C:\\Music\\single.flac",
    action: "skip",
    risk: "path_too_long",
    reason: "path_too_long:320:240",
  };
  const existingLog = {
    id: "operation-existing",
    execution_id: "execution-1",
    sequence_no: 5,
    source_path: longPath,
    target_path: "C:\\Music\\existing.flac",
    action: "copy_source_retained",
    result: "failed",
    error: "target_already_exists",
    created_at: 1,
  };
  const mounted = await mount(React.createElement(
    React.Fragment,
    null,
    React.createElement(PlanConflictCard, { planId: "plan-1", item: retryItem }),
    React.createElement(PlanConflictCard, { planId: "plan-1", item: singletonItem }),
    React.createElement(ExistingTargetConflict, { log: existingLog }),
  ));
  try {
    const cards = [...mounted.container.querySelectorAll('[aria-label="ファイル衝突の詳細"]')];
    assert.equal(cards.length, 2);
    assert.match(cards[0].textContent, /衝突相手を読み込んでいます/);
    assert.equal(cards[0].getAttribute("aria-label"), "ファイル衝突の詳細");

    await act(async () => rejectInitial(new Error("detail temporarily unavailable")));
    await waitFor(() => buttonByText(cards[0], "再試行"), "the conflict retry action");
    await act(async () => buttonByText(cards[0], "再試行").click());
    await waitFor(() => cards[0].textContent.includes("other-1.flac"), "the retried conflict detail");
    assert.equal(cards[0].textContent.includes("other-2.flac"), false);
    await act(async () => buttonByText(cards[0], "ほか1件を表示（全2件）").click());
    assert.match(cards[0].textContent, /other-2\.flac/);
    assert.equal(retryCalls, 2);
    const fullLongPath = [...cards[0].querySelectorAll("code")]
      .find(value => value.getAttribute("title") === longPath);
    assert.equal(fullLongPath?.textContent, longPath);

    await waitFor(() => cards[1].textContent.includes("single.flac"), "the singleton detail");
    const singletonLabels = [...cards[1].querySelectorAll("span")].map(value => value.textContent);
    assert.equal(singletonLabels.includes("衝突相手"), false);

    const existing = mounted.container.querySelector('[aria-label="既存ファイルとの衝突"]');
    assert.match(existing.textContent, /既存ファイルとの衝突/);
    assert.match(existing.textContent, /existing\.flac/);
    assert.equal(existing.querySelectorAll(".diagnostic-path").length, 3);
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
});

test("mounted App authorizes a raw manual target then revises with only the one-shot capability", async () => {
  const environment = installDom();
  const calls = [];
  const restoredPlan = {
    job_id: "job-restored-plan",
    workflow_generation: 1,
    event_seq: 2,
    kind: "plan",
    phase: "planning",
    status: "completed",
    input_id: "scan-1",
    run_id: "plan-1",
    scan_id: "scan-1",
    result: { id: "plan-1", success: 1, skipped: 0, failed: 0 },
  };
  const item = {
    id: "item-1",
    conflict_member_count: 0,
    ordinal: 1,
    source_path: "C:\\Incoming\\track.flac",
    target_path: "C:\\Music\\Artist\\track.flac",
    action: "move",
    risk: "none",
  };
  mockIPC((command, payload) => {
    calls.push({ command, payload: payload ? { ...payload } : payload });
    if (command === "list_active_jobs") return [restoredPlan];
    if (command === "list_plan_items") {
      return {
        items: [item],
        total: 1,
        filtered_total: 1,
        next_cursor: null,
        counts: { moves: 1, skips: 0, needs_attention: 0, conflicts: 0, invalid_target: 0, metadata_missing: 0, path_too_long: 0 },
      };
    }
    if (command === "authorize_plan_target") {
      return { target_capability_id: "opaque-capability", display: payload.target };
    }
    if (command === "revise_plan_target") {
      return {
        job_id: "job-revision",
        workflow_generation: 2,
        event_seq: 1,
        kind: "plan_revision",
        phase: "planning",
        status: "running",
        input_id: "plan-1",
        scan_id: "scan-1",
        plan_id: "plan-1",
      };
    }
    return defaultIpc(command);
  }, { shouldMockEvents: true });

  const mounted = await mount(React.createElement(App));
  try {
    const edit = await waitFor(
      () => buttonByText(mounted.container, "移動先を変更"),
      "the restored plan item",
    );
    await act(async () => edit.click());
    const dialog = await waitFor(
      () => mounted.container.querySelector("dialog[open]"),
      "the manual-target dialog",
    );
    assert.equal(dialog.getAttribute("aria-labelledby"), "target-edit-title");
    const input = dialog.querySelector("input");
    assert.equal(document.activeElement, input);
    assert.equal(input.value, "", "a lossy display path must never be replayed as edit input");
    const manualTarget = `C:\\Organized\\${"長い階層\\".repeat(30)}track.flac`;
    await act(async () => setControlledInput(input, manualTarget));
    assert.equal(input.value, manualTarget);
    await act(async () => buttonByText(dialog, "改訂Planを作成").click());
    await waitFor(
      () => calls.some(call => call.command === "revise_plan_target"),
      "the authorized plan revision",
    );

    const authorization = calls.find(call => call.command === "authorize_plan_target");
    const revision = calls.find(call => call.command === "revise_plan_target");
    assert.deepEqual(authorization.payload, {
      planId: "plan-1",
      planItemId: "item-1",
      target: manualTarget,
      expectedWorkflowGeneration: 1,
    });
    assert.deepEqual(revision.payload, {
      planId: "plan-1",
      planItemId: "item-1",
      targetCapabilityId: "opaque-capability",
      expectedWorkflowGeneration: 1,
    });
    assert.equal(Object.hasOwn(revision.payload, "target"), false);
    const readyProbe = calls.find(call => call.command === "desktop_backend_ready");
    assert.deepEqual(readyProbe.payload ?? {}, {});
    assert.equal(calls.some(call => call.command === "desktop_database_path"), false);
    assert.equal(localStorage.getItem("mfb.active-job"), "job-revision");
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
});

test("mounted App selects a persisted image candidate by opaque ordinal without replaying its display path", async () => {
  const environment = installDom();
  const calls = [];
  const restoredPlan = {
    job_id: "job-restored-image-plan",
    workflow_generation: 4,
    event_seq: 2,
    kind: "plan",
    phase: "planning",
    status: "completed",
    input_id: "scan-image",
    run_id: "plan-image",
    scan_id: "scan-image",
    result: { id: "plan-image", success: 0, skipped: 1, failed: 0 },
  };
  const item = {
    id: "image-item",
    conflict_group_id: "image-group",
    conflict_member_count: 1,
    ordinal: 9,
    source_path: "C:\\Incoming\\表示�.jpg",
    action: "skip",
    risk: "conflict",
    reason: "companion_target_ambiguous",
  };
  mockIPC((command, payload) => {
    calls.push({ command, payload: payload ? { ...payload } : payload });
    if (command === "list_active_jobs") return [restoredPlan];
    if (command === "list_plan_items") {
      return {
        items: [item],
        total: 1,
        filtered_total: 1,
        next_cursor: null,
        counts: { moves: 0, skips: 1, needs_attention: 1, conflicts: 1, invalid_target: 0, metadata_missing: 0, path_too_long: 0 },
      };
    }
    if (command === "get_plan_conflict_detail") {
      return {
        id: "image-group",
        kind: "companion_target_ambiguous",
        target_path: "",
        members: [],
        candidates: [{
          ordinal: 17,
          target_path: "C:\\Music\\表示�\\Album",
          members: [{ item_id: "music-item", ordinal: 2, source_path: "C:\\Incoming\\track.flac" }],
        }],
      };
    }
    if (command === "authorize_plan_conflict_candidate") {
      return { target_capability_id: "opaque-candidate-capability" };
    }
    if (command === "revise_plan_target") {
      return {
        job_id: "job-image-revision",
        workflow_generation: 5,
        event_seq: 1,
        kind: "plan_revision",
        phase: "planning",
        status: "running",
        input_id: "plan-image",
        scan_id: "scan-image",
        plan_id: "plan-image",
      };
    }
    return defaultIpc(command);
  }, { shouldMockEvents: true });

  const mounted = await mount(React.createElement(App));
  try {
    const open = await waitFor(
      () => buttonByText(mounted.container, "移動先候補を確認（1件）"),
      "the image candidate disclosure",
    );
    await act(async () => open.click());
    const select = await waitFor(
      () => buttonByText(mounted.container, "この移動先を選択"),
      "the persisted image candidate",
    );
    await act(async () => select.click());
    await waitFor(
      () => calls.some(call => call.command === "revise_plan_target"),
      "the candidate-backed revision",
    );

    const authorization = calls.find(call => call.command === "authorize_plan_conflict_candidate");
    const revision = calls.find(call => call.command === "revise_plan_target");
    assert.deepEqual(authorization.payload, {
      planId: "plan-image",
      planItemId: "image-item",
      conflictGroupId: "image-group",
      candidateOrdinal: 17,
      expectedWorkflowGeneration: 4,
    });
    assert.equal(Object.hasOwn(authorization.payload, "target"), false);
    assert.equal(Object.hasOwn(authorization.payload, "sourcePath"), false);
    assert.deepEqual(revision.payload, {
      planId: "plan-image",
      planItemId: "image-item",
      targetCapabilityId: "opaque-candidate-capability",
      expectedWorkflowGeneration: 4,
    });
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
});

async function exerciseTerminalJob(status) {
  const environment = installDom();
  const running = {
    job_id: `job-${status}`,
    workflow_generation: 1,
    event_seq: 1,
    kind: "plan",
    phase: "planning",
    status: "running",
    input_id: "scan-1",
    scan_id: "scan-1",
  };
  const terminal = {
    ...running,
    event_seq: 2,
    status,
    run_id: status === "completed" ? "plan-completed" : undefined,
    result: status === "completed"
      ? { id: "plan-completed", success: 1, skipped: 0, failed: 0 }
      : undefined,
    error: status === "failed" ? "deterministic job failure" : undefined,
  };
  mockIPC((command) => {
    if (command === "list_active_jobs") return [running];
    if (command === "get_job_snapshot") return terminal;
    return defaultIpc(command);
  }, { shouldMockEvents: true });
  const mounted = await mount(React.createElement(App));
  try {
    const expected = status === "completed" ? "Plan作成済み" : "Scan完了・Plan作成待ち";
    await waitFor(
      () => mounted.container.querySelector(".workflow-status strong")?.textContent === expected,
      `${status} job to settle`,
    );
    await waitFor(
      () => mounted.container.querySelector(`[data-job-status="${status}"]`),
      `${status} job outcome to be announced`,
    );
    return {
      workflowStatus: mounted.container.querySelector(".workflow-status strong")?.textContent,
      alert: mounted.container.querySelector("[role=alert]")?.textContent,
      outcome: mounted.container.querySelector(`[data-job-status="${status}"]`)?.textContent,
      result: mounted.container.querySelector("output")?.textContent,
      rememberedJob: localStorage.getItem("mfb.active-job"),
    };
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
}

test("mounted App visibly settles successful, failed, and cancelled jobs", async () => {
  const completed = await exerciseTerminalJob("completed");
  assert.equal(completed.workflowStatus, "Plan作成済み");
  assert.match(completed.result, /Scan:/);
  assert.equal(completed.outcome, "Plan作成が完了しました。");
  assert.equal(completed.alert, undefined);
  assert.equal(completed.rememberedJob, null);

  const failed = await exerciseTerminalJob("failed");
  assert.equal(failed.workflowStatus, "Scan完了・Plan作成待ち");
  assert.match(failed.outcome, /^Plan作成に失敗しました: 詳細不明のエラー/);
  assert.equal(failed.outcome.includes("deterministic job failure"), false);
  assert.equal(failed.alert, failed.outcome);
  assert.equal(failed.rememberedJob, null);

  const cancelled = await exerciseTerminalJob("cancelled");
  assert.equal(cancelled.workflowStatus, "Scan完了・Plan作成待ち");
  assert.equal(cancelled.outcome, "Plan作成は取り消されました。");
  assert.equal(cancelled.alert, undefined);
  assert.equal(cancelled.rememberedJob, null);
});

test("mounted App ignores stale snapshots and a late failure after a newer terminal snapshot", async () => {
  const environment = installDom();
  const running = {
    job_id: "job-sequenced",
    workflow_generation: 1,
    event_seq: 1,
    kind: "plan",
    phase: "planning",
    status: "running",
    input_id: "scan-1",
    scan_id: "scan-1",
  };
  let calls = 0;
  let resolveLate;
  mockIPC((command) => {
    if (command === "list_active_jobs") return [running];
    if (command === "get_job_snapshot") {
      calls += 1;
      if (calls === 1) {
        return new Promise(resolve => { resolveLate = resolve; });
      }
      if (calls === 2) {
        return { ...running, workflow_generation: 0, event_seq: 99 };
      }
      return {
        ...running,
        event_seq: 3,
        status: "completed",
        run_id: "plan-current",
        result: { id: "plan-current", success: 1, skipped: 0, failed: 0 },
      };
    }
    return defaultIpc(command);
  }, { shouldMockEvents: true });
  const mounted = await mount(React.createElement(App));
  try {
    await waitFor(
      () => mounted.container.querySelector(".workflow-status strong")?.textContent === "Plan作成済み",
      "the current terminal snapshot",
    );
    assert.ok(calls >= 3, "a stale generation must not stop polling");
    await act(async () => {
      resolveLate({
        ...running,
        event_seq: 2,
        status: "failed",
        error: "late failure must stay invisible",
      });
      await new Promise(resolve => setTimeout(resolve, 30));
    });
    assert.equal(mounted.container.querySelector(".workflow-status strong")?.textContent, "Plan作成済み");
    assert.equal(mounted.container.querySelector("[role=alert]"), null);
    assert.match(mounted.container.textContent, /Plan ID plan-current/);
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
});

test("mounted App archives then explicitly purges a workflow with root-bound confirmations", async () => {
  const environment = installDom();
  const calls = [];
  let archived = false;
  let purged = false;
  const archivePath = {
    schema_version: 1,
    role: "archive_path",
    display: "C:\\State\\history-archives",
    display_lossy: false,
    encoding: "utf16le-v1",
    raw_base64: "QwA6AFwAUwB0AGEAdABlAA==",
  };
  const historyRow = () => ({
    id: "scan-root",
    kind: "scan",
    logical_run_id: "scan-root",
    attempt_no: 1,
    actor: "desktop",
    status: archived ? "archived" : "completed",
    started_at: 1,
    finished_at: 2,
    root_scan_id: "scan-root",
    success: 2,
    skipped: 0,
    failed: 0,
  });
  mockIPC((command, payload) => {
    calls.push({ command, payload: payload ? { ...payload } : payload });
    if (command === "list_history") return purged ? [] : [historyRow()];
    if (command === "get_workflow_capabilities") return workflowCapabilities();
    if (command === "history_cleanup_preview") {
      return {
        root_scan_id: "scan-root",
        plans: 1,
        executions: 2,
        logs: 7,
        blocked: false,
        protection_reasons: [],
        estimated_bytes: 4096,
        archive_path: archivePath,
        archived,
      };
    }
    if (command === "archive_history") {
      archived = true;
      return {
        archive_id: "archive-1",
        root_scan_id: "scan-root",
        archive_path: archivePath,
        schema_version: 3,
        record_count: 12,
        byte_count: 4096,
        digest_algorithm: "sha256",
        sha256: "a".repeat(64),
        verified: true,
        already_archived: false,
      };
    }
    if (command === "delete_history") {
      purged = true;
      return null;
    }
    return defaultIpc(command);
  }, { shouldMockEvents: true });

  const mounted = await mount(React.createElement(App));
  try {
    let row = await waitFor(
      () => mounted.container.querySelector(".history-entry"),
      "the completed workflow history",
    );
    await act(async () => row.click());
    const archiveButton = await waitFor(
      () => {
        const button = buttonByText(mounted.container, "履歴を整理");
        return button && !button.disabled ? button : undefined;
      },
      "the server-authorized archive action",
    );
    await act(async () => archiveButton.click());
    let dialog = await waitFor(
      () => mounted.container.querySelector("dialog[open]"),
      "the archive confirmation",
    );
    assert.match(dialog.textContent, /実ファイルと履歴DBのrecordは削除しません/);
    await act(async () => buttonByText(dialog, "アーカイブを作成").click());
    await waitFor(
      () => calls.some(call => call.command === "archive_history"),
      "the verified archive command",
    );
    const archiveCall = calls.find(call => call.command === "archive_history");
    assert.deepEqual(archiveCall.payload, {
      kind: "scan",
      runId: "scan-root",
      confirm: "scan-root",
      expectedWorkflowGeneration: 0,
    });

    row = await waitFor(
      () => mounted.container.querySelector(".history-entry"),
      "the archived workflow history",
    );
    await act(async () => row.click());
    const purge = await waitFor(
      () => buttonByText(mounted.container, "検証済み履歴を削除"),
      "the separate dangerous purge action",
    );
    await waitFor(() => !purge.disabled, "the server-authorized purge action");
    await act(async () => purge.click());
    dialog = await waitFor(
      () => mounted.container.querySelector("dialog[open]"),
      "the purge confirmation",
    );
    assert.match(dialog.textContent, /検証済みJSONLアーカイブ/);
    assert.match(dialog.textContent, /音楽ファイルは削除しません/);
    await act(async () => buttonByText(dialog, "履歴を完全削除").click());
    await waitFor(
      () => calls.some(call => call.command === "delete_history"),
      "the confirmed history purge",
    );
    const purgeCall = calls.find(call => call.command === "delete_history");
    assert.deepEqual(purgeCall.payload, {
      kind: "scan",
      runId: "scan-root",
      confirm: "scan-root",
      expectedWorkflowGeneration: 1,
    });
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
});

test("mounted App binds destructive Apply confirmation to the persisted Plan ID", async () => {
  const environment = installDom();
  const calls = [];
  const restoredPlan = {
    job_id: "job-confirmed-plan",
    workflow_generation: 3,
    event_seq: 2,
    kind: "plan",
    phase: "planning",
    status: "completed",
    input_id: "scan-confirm",
    run_id: "plan-confirm",
    scan_id: "scan-confirm",
    result: { id: "plan-confirm", success: 1, skipped: 0, failed: 0 },
  };
  mockIPC((command, payload) => {
    calls.push({ command, payload: payload ? { ...payload } : payload });
    if (command === "list_active_jobs") return [restoredPlan];
    if (command === "get_workflow_capabilities") {
      return workflowCapabilities({ generation: 3, planId: "plan-confirm" });
    }
    if (command === "apply_plan") {
      return {
        job_id: "job-apply-confirm",
        workflow_generation: 4,
        event_seq: 1,
        kind: "apply",
        phase: "applying",
        status: "running",
        input_id: "plan-confirm",
        scan_id: "scan-confirm",
        plan_id: "plan-confirm",
      };
    }
    return defaultIpc(command);
  }, { shouldMockEvents: true });

  const mounted = await mount(React.createElement(App));
  try {
    const apply = await waitFor(
      () => {
        const button = buttonByText(mounted.container, "Apply");
        return button && !button.disabled ? button : undefined;
      },
      "the restored Plan Apply action",
    );
    await act(async () => apply.click());
    const dialog = await waitFor(
      () => mounted.container.querySelector("dialog[open]"),
      "the Apply confirmation",
    );
    await act(async () => buttonByText(dialog, "Applyを実行").click());
    await waitFor(
      () => calls.some(call => call.command === "apply_plan"),
      "the confirmed Apply command",
    );
    assert.deepEqual(calls.find(call => call.command === "apply_plan").payload, {
      planId: "plan-confirm",
      execute: true,
      confirm: "plan-confirm",
      expectedWorkflowGeneration: 3,
    });
  } finally {
    await mounted.unmount();
    clearMocks();
    environment.restore();
  }
});
