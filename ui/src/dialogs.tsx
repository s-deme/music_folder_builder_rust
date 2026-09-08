import { FormEvent, useEffect, useRef, useState } from "react";

type FocusTarget = { focus: () => void };
type CancelEvent = { preventDefault: () => void };

export function openModalAndFocus(
  dialog: { open: boolean; showModal: () => void },
  initial: FocusTarget | undefined,
): void {
  if (!dialog.open) dialog.showModal();
  initial?.focus();
}

export function returnFocus(
  opener: FocusTarget | undefined,
  schedule: (callback: () => void) => unknown = callback => window.setTimeout(callback, 0),
): void {
  schedule(() => opener?.focus());
}

export function cancelModal(event: CancelEvent, close: () => void): void {
  event.preventDefault();
  close();
}

export async function performDialogAction(
  action: () => void | Promise<void>,
  onSuccess: () => void,
  onFailure: (reason: unknown) => void,
): Promise<void> {
  try {
    await action();
    onSuccess();
  } catch (reason) {
    onFailure(reason);
  }
}

export type ConfirmationRequest = {
  title: string;
  message: string;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void | Promise<void>;
};

export function ConfirmDialog({
  request,
  onClose,
}: {
  request?: ConfirmationRequest;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const openerRef = useRef<HTMLElement>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (request && !dialog.open) {
      openerRef.current = document.activeElement as HTMLElement | undefined;
      setPending(false);
      setError(undefined);
      openModalAndFocus(dialog, cancelRef.current ?? undefined);
    } else if (!request && dialog.open) {
      dialog.close();
    }
  }, [request]);

  function close() {
    if (pending) return;
    onClose();
    returnFocus(openerRef.current);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!request || pending) return;
    setPending(true);
    setError(undefined);
    await performDialogAction(
      request.onConfirm,
      () => {
        onClose();
        returnFocus(openerRef.current);
      },
      reason => {
        setError(String(reason));
        setPending(false);
      },
    );
  }

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="confirmation-title"
      aria-describedby="confirmation-message"
      onCancel={event => {
        cancelModal(event, close);
      }}
      onClose={() => {
        if (request && !pending) onClose();
      }}
    >
      <form onSubmit={event => void submit(event)}>
        <h2 id="confirmation-title">{request?.title}</h2>
        <p id="confirmation-message">{request?.message}</p>
        {error && <p className="error" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button ref={cancelRef} type="button" className="secondary" disabled={pending} onClick={close}>キャンセル</button>
          <button type="submit" className={request?.danger ? "danger" : undefined} disabled={pending}>
            {pending ? "処理中…" : request?.confirmLabel}
          </button>
        </div>
      </form>
    </dialog>
  );
}

export type TargetEditRequest = {
  currentTarget: string;
  onSubmit: (target: string) => void | Promise<void>;
};

export function TargetEditDialog({
  request,
  onClose,
}: {
  request?: TargetEditRequest;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const openerRef = useRef<HTMLElement>();
  const [target, setTarget] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (request && !dialog.open) {
      openerRef.current = document.activeElement as HTMLElement | undefined;
      setTarget(request.currentTarget);
      setPending(false);
      setError(undefined);
      openModalAndFocus(dialog, inputRef.current ?? undefined);
      inputRef.current?.select();
    } else if (!request && dialog.open) {
      dialog.close();
    }
  }, [request]);

  function close() {
    if (pending) return;
    onClose();
    returnFocus(openerRef.current);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!request || pending || !target.trim()) return;
    setPending(true);
    setError(undefined);
    await performDialogAction(
      () => request.onSubmit(target.trim()),
      () => {
        onClose();
        returnFocus(openerRef.current);
      },
      reason => {
        setError(String(reason));
        setPending(false);
      },
    );
  }

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="target-edit-title"
      aria-describedby="target-edit-description"
      onCancel={event => {
        cancelModal(event, close);
      }}
      onClose={() => {
        if (request && !pending) onClose();
      }}
    >
      <form onSubmit={event => void submit(event)}>
        <h2 id="target-edit-title">移動先を変更</h2>
        <p id="target-edit-description">元のPlanは変更せず、新しい改訂Planを作成します。</p>
        <label>新しい移動先<input ref={inputRef} value={target} onChange={event => setTarget(event.target.value)} /></label>
        {error && <p className="error" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button type="button" className="secondary" disabled={pending} onClick={close}>キャンセル</button>
          <button type="submit" disabled={pending || !target.trim()}>{pending ? "作成中…" : "改訂Planを作成"}</button>
        </div>
      </form>
    </dialog>
  );
}
