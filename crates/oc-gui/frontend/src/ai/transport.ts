/**
 * The one place the agent loop reaches the host.
 *
 * In the app this is `invoke` from Tauri, unchanged. A headless driver (a
 * benchmark, a test) may hand in its own transport before the loop runs, and
 * that is the only difference: same command names, same arguments, same
 * results. Nothing here inspects or rewrites them.
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

let installed: Invoke | null = null;

/** Installs a transport for a headless run; `null` restores the app path. */
export function setInvoke(next: Invoke | null): void {
  installed = next;
}

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return installed ? (installed as Invoke)<T>(command, args) : tauriInvoke<T>(command, args);
}
