/**
 * Typed wrappers for the network preference commands.
 *
 * The backend owns the saved proxy and the probe policy; this module only
 * keeps the wire shapes in one place.
 */
import { invoke } from "@tauri-apps/api/core";

export type ProxyMode = "system" | "manual" | "none";

export interface NetworkSettings {
  proxyMode: ProxyMode;
  proxyUrl: string;
}

export interface ProbeResult {
  ok: boolean;
  status?: number;
  ms: number;
  bytes?: number;
  error?: string;
}

export type RouteKind = "system" | "direct" | "proxy";

export interface RouteProbe {
  kind: RouteKind;
  url: string | null;
  ok: boolean;
  ms: number;
  error: string | null;
}

export interface NetworkDiagnosis {
  current: RouteProbe;
  routes: RouteProbe[];
}

export function loadNetworkSettings(): Promise<NetworkSettings> {
  return invoke<NetworkSettings>("network_settings");
}

export function saveNetworkSettings(settings: NetworkSettings): Promise<NetworkSettings> {
  return invoke<NetworkSettings>("network_save", { settings });
}

export function testNetwork(mode: ProxyMode, url: string): Promise<ProbeResult> {
  return invoke<ProbeResult>("network_test", { proxyMode: mode, proxyUrl: url });
}

export function diagnoseNetwork(): Promise<NetworkDiagnosis> {
  return invoke<NetworkDiagnosis>("network_diagnose");
}
