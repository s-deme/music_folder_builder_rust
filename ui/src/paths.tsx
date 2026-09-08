import React from "react";
import type { LosslessPathEnvelope } from "./model";

export function pathDisplay(path: LosslessPathEnvelope | undefined, fallback = ""): string {
  return path?.display ?? fallback;
}

export function losslessClipboardValue(
  path: LosslessPathEnvelope | undefined,
  fallback = "",
): string {
  return path ? JSON.stringify(path) : fallback;
}

export function PathValue({
  path,
  fallback,
  copy = false,
}: {
  path?: LosslessPathEnvelope;
  fallback?: string;
  copy?: boolean;
}) {
  const display = pathDisplay(path, fallback);
  return (
    <span className="lossless-path-value">
      <code title={display}>{display}</code>
      {path?.display_lossy && (
        <span className="path-lossy-badge" role="note">
          表示できない文字を含む
        </span>
      )}
      {copy && (
        <button
          type="button"
          className="secondary compact"
          onClick={() => void navigator.clipboard.writeText(losslessClipboardValue(path, fallback))}
        >
          {path ? "可逆形式でコピー" : "コピー"}
        </button>
      )}
    </span>
  );
}
