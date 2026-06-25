// Backend abstraction: the desktop app talks to the Rust core over Tauri IPC; the browser /
// installed PWA talks to the same protocol stack compiled to WebAssembly. `invoke` routes by
// environment so the rest of the app (`api.ts`) stays backend-agnostic. See
// docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { browserInvoke } from "./browserBackend";

/** True when running inside the Tauri desktop shell (vs a plain browser / installed PWA). */
export function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>)
  );
}

/** Invoke a backend command — Tauri IPC on desktop, the wasm core in the browser. */
export function invoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  return isTauri() ? tauriInvoke<T>(cmd, args) : browserInvoke<T>(cmd, args);
}
