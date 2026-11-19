/**
 * The connection diagnosis: a working proxy is offered first, then a direct
 * connection, and a failed saved route still reports its error.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "./setup";
import { networkStore, type NetworkDiagnosis } from "../stores/network";

function route(
  kind: "system" | "direct" | "proxy",
  ok: boolean,
  ms: number,
  url: string | null = null,
  error: string | null = null,
) {
  return { kind, ok, ms, url, error };
}

async function diagnoseWith(diagnosis: NetworkDiagnosis): Promise<void> {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "network_diagnose") return diagnosis;
    return null;
  });
  await networkStore.diagnose();
}

beforeEach(() => {
  invokeMock.mockReset();
  networkStore.reset();
});

describe("network diagnosis", () => {
  it("offers the fastest working proxy when the saved route fails", async () => {
    await diagnoseWith({
      current: route("system", false, 8, null, "tls handshake eof"),
      routes: [
        route("proxy", true, 40, "http://127.0.0.1:7897"),
        route("proxy", true, 10, "http://127.0.0.1:7891"),
        route("direct", false, 2000, null, "timeout"),
      ],
    });
    expect(networkStore.offer()).toEqual({
      kind: "proxy",
      url: "http://127.0.0.1:7891",
      ms: 10,
    });
  });

  it("offers a direct connection when no proxy works", async () => {
    await diagnoseWith({
      current: route("system", false, 8, null, "tls handshake eof"),
      routes: [
        route("direct", true, 700, null),
        route("proxy", false, 30, "http://127.0.0.1:7897", "HTTP 502"),
      ],
    });
    expect(networkStore.offer()).toEqual({ kind: "direct", url: null, ms: 700 });
  });

  it("offers nothing when the saved route works", async () => {
    await diagnoseWith({
      current: route("proxy", true, 25, "http://127.0.0.1:7890"),
      routes: [
        route("proxy", true, 25, "http://127.0.0.1:7890"),
        route("direct", false, 2000, null, "timeout"),
      ],
    });
    expect(networkStore.offer()).toBeNull();
  });

  it("reports a proxy that is running but cannot reach the index", async () => {
    await diagnoseWith({
      current: route("system", false, 8, null, "tls handshake eof"),
      routes: [
        route("direct", false, 2000, null, "timeout"),
        route("proxy", false, 40, "http://127.0.0.1:7897", "tls handshake eof"),
      ],
    });
    expect(networkStore.offer()).toBeNull();
    expect(networkStore.failingProxies()).toEqual([
      route("proxy", false, 40, "http://127.0.0.1:7897", "tls handshake eof"),
    ]);
  });

  it("surfaces a diagnosis command failure", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "network_diagnose") throw new Error("backend unavailable");
      return null;
    });
    await networkStore.diagnose();
    expect(networkStore.diagnoseError()).toContain("backend unavailable");
    expect(networkStore.offer()).toBeNull();
  });
});
