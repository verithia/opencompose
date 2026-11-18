/**
 * Network preferences: the proxy every outbound client follows.
 *
 * The backend owns the value (`gui-settings.yaml`); this store is the page's
 * view, the connection diagnosis and the verified offer shown when a fetch
 * fails.
 */
import { createMemo, createSignal } from "solid-js";
import * as api from "./networkApi";
import type {
  NetworkDiagnosis,
  NetworkSettings,
  ProbeResult,
  ProxyMode,
  RouteProbe,
} from "./networkApi";

export type {
  NetworkDiagnosis,
  NetworkSettings,
  ProbeResult,
  ProxyMode,
  RouteKind,
  RouteProbe,
} from "./networkApi";

/** A route worth applying: a working proxy candidate or a direct connection. */
export interface RouteOffer {
  kind: "proxy" | "direct";
  url: string | null;
  ms: number;
}

const [settings, setSettings] = createSignal<NetworkSettings>({
  proxyMode: "system",
  proxyUrl: "",
});
const [probe, setProbe] = createSignal<ProbeResult | null>(null);
const [probing, setProbing] = createSignal(false);
const [diagnosis, setDiagnosis] = createSignal<NetworkDiagnosis | null>(null);
const [diagnosing, setDiagnosing] = createSignal(false);
const [diagnoseError, setDiagnoseError] = createSignal<string | null>(null);

/**
 * The route to offer, or `null` when the saved route already works.
 *
 * A working local proxy comes first: where a proxy runs, the other hosts the
 * app reaches (plugin downloads, sample markets, AI providers) usually need it
 * too, while the index alone may answer directly. A direct connection is the
 * fallback.
 */
const offer = createMemo<RouteOffer | null>(() => {
  const current = diagnosis();
  if (!current || current.current.ok) return null;
  const working = current.routes.filter((route) => route.ok);
  const proxy = working
    .filter((route) => route.kind === "proxy" && route.url)
    .sort((left, right) => left.ms - right.ms)[0];
  if (proxy?.url) return { kind: "proxy", url: proxy.url, ms: proxy.ms };
  const direct = working.find((route) => route.kind === "direct");
  return direct ? { kind: "direct", url: null, ms: direct.ms } : null;
});

/** Proxies that answered CONNECT but could not reach the index. */
const failingProxies = createMemo(() =>
  (diagnosis()?.routes ?? []).filter((route) => route.kind === "proxy" && !route.ok),
);

async function load(): Promise<void> {
  try {
    setSettings(await api.loadNetworkSettings());
  } catch {
    // A preview without Tauri keeps the default.
  }
}

async function save(next: NetworkSettings): Promise<void> {
  setSettings(await api.saveNetworkSettings(next));
}

async function test(mode: ProxyMode, url: string): Promise<void> {
  setProbing(true);
  setProbe(null);
  try {
    setProbe(await api.testNetwork(mode, url));
  } catch (error) {
    setProbe({ ok: false, ms: 0, error: String(error) });
  } finally {
    setProbing(false);
  }
}

/** Test the saved route, a direct route and every local proxy candidate. */
async function diagnose(): Promise<void> {
  if (diagnosing()) return;
  setDiagnosing(true);
  setDiagnoseError(null);
  try {
    setDiagnosis(await api.diagnoseNetwork());
  } catch (error) {
    setDiagnosis(null);
    setDiagnoseError(String(error));
  } finally {
    setDiagnosing(false);
  }
}

/** Apply one tested route and persist it as the saved choice. */
async function applyRoute(route: RouteProbe): Promise<void> {
  if (route.kind === "proxy" && route.url) {
    await save({ proxyMode: "manual", proxyUrl: route.url });
  } else if (route.kind === "direct") {
    await save({ proxyMode: "none", proxyUrl: settings().proxyUrl });
  } else {
    await save({ proxyMode: "system", proxyUrl: settings().proxyUrl });
  }
}

/** Apply the verified offer; nothing happens when no route works. */
async function applyOffer(target = offer()): Promise<void> {
  if (!target) return;
  await applyRoute({
    kind: target.kind,
    url: target.url,
    ok: true,
    ms: target.ms,
    error: null,
  });
  setDiagnosis(null);
}

function reset(): void {
  setSettings({ proxyMode: "system", proxyUrl: "" });
  setProbe(null);
  setProbing(false);
  setDiagnosis(null);
  setDiagnosing(false);
  setDiagnoseError(null);
}

export const networkStore = {
  settings,
  probe,
  probing,
  diagnosis,
  diagnosing,
  diagnoseError,
  offer,
  failingProxies,
  load,
  save,
  test,
  diagnose,
  applyRoute,
  applyOffer,
  reset,
};
