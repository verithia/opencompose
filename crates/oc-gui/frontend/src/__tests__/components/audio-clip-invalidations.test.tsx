import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createComputed, createRoot, createSignal } from "solid-js";
import { render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeAudioClip, makeSnapshot } from "../setup";
import { AudioClipBlock } from "../../components/AudioClipBlock";
import { loadPeaks, peaksFor, peaksMissing, resetPeaks } from "../../stores/audioPeaks";
import { store } from "../../stores/project";
import { themeStore } from "../../stores/theme";

function payload(): string {
  const bytes = new Uint8Array(50), view = new DataView(bytes.buffer);
  bytes.set([0x4f, 0x43, 0x50, 0x4b]); view.setUint8(4, 1);
  view.setUint32(14, 48_000, true); view.setBigUint64(18, 256n, true);
  view.setUint32(26, 20, true); view.setUint32(30, 256, true); view.setUint32(34, 1, true);
  [-0.5, 0.5, 0.3].forEach((value, index) => view.setFloat32(38 + index * 4, value, true));
  return btoa(String.fromCharCode(...bytes));
}
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
beforeEach(() => { resetPeaks(); invokeMock.mockClear(); });
afterEach(() => { vi.restoreAllMocks(); resetPeaks(); });

describe("file-scoped waveform invalidation", () => {
  it("notifies only consumers of the completed or failed file", async () => {
    const a = deferred<string>(), b = deferred<string>();
    invokeMock.mockImplementation((cmd: string, args?: { file: string }) => cmd === "audio_clip_peaks" ? args?.file === "a.wav" ? a.promise : b.promise : undefined);
    const runs = { a: 0, b: 0 };
    const dispose = createRoot((dispose) => {
      createComputed(() => { peaksFor("a.wav"); peaksMissing("a.wav"); runs.a += 1; });
      createComputed(() => { peaksFor("b.wav"); peaksMissing("b.wav"); runs.b += 1; });
      return dispose;
    });
    try {
      const pa = loadPeaks("a.wav"), pb = loadPeaks("b.wav"), initial = { ...runs };
      expect(loadPeaks("a.wav")).toBe(pa);
      a.resolve(payload()); await pa;
      expect(runs).toEqual({ a: initial.a + 1, b: initial.b });
      expect(peaksFor("a.wav")?.frames).toBe(256);
      b.reject(new Error("missing")); await pb;
      expect(runs).toEqual({ a: initial.a + 1, b: initial.b + 1 });
      expect(peaksMissing("b.wav")).toBe(true);
      resetPeaks();
      expect(runs).toEqual({ a: initial.a + 2, b: initial.b + 2 });
      expect(peaksFor("a.wav")).toBeNull();
      expect(peaksMissing("b.wav")).toBe(false);
    } finally { dispose(); }
  });
  it("rejects stale completions without deleting a new same-file pending request", async () => {
    const old = deferred<string>(), current = deferred<string>();
    invokeMock.mockImplementationOnce(() => old.promise).mockImplementationOnce(() => current.promise);
    const first = loadPeaks("same.wav"); resetPeaks(); const second = loadPeaks("same.wav");
    old.resolve(payload()); await first;
    expect(peaksFor("same.wav")).toBeNull(); expect(loadPeaks("same.wav")).toBe(second);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    current.resolve(payload()); await second; expect(peaksFor("same.wav")?.frames).toBe(256);
  });
  it("repaints matching canvases only and refreshes colors on theme/accent changes", async () => {
    const a = deferred<string>(), b = deferred<string>();
    invokeMock.mockImplementation((cmd: string, args?: { file: string }) => cmd === "audio_clip_peaks" ? args?.file === "a.wav" ? a.promise : b.promise : makeSnapshot());
    await store.initProject(); themeStore.setTheme("default/light");
    const contexts = new Map<HTMLCanvasElement, ReturnType<typeof vi.fn>>();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(function (this: HTMLCanvasElement) {
      const paint = contexts.get(this) ?? vi.fn(); contexts.set(this, paint);
      return { clearRect: vi.fn(), fillRect: vi.fn(), beginPath: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn(), setTransform: paint } as unknown as CanvasRenderingContext2D;
    });
    const computed = vi.spyOn(window, "getComputedStyle").mockImplementation((element) => {
      const color = (element as HTMLCanvasElement).style.color;
      return { color: color.includes("--text-primary") ? themeStore.cssTheme() === "ivory" ? "rgb(10, 20, 30)" : "rgb(230, 240, 250)"
        : color.includes("--bg-void") ? "rgb(0, 0, 0)" : color } as CSSStyleDeclaration;
    });
    const [accent, setAccent] = createSignal("rgb(80, 100, 120)"), [scale, setScale] = createSignal(24);
    const props = { color: "rgb(20, 30, 40)", height: 64, selected: false, tooltipContent: () => null,
      beatFromClientX: () => 0, trackAtClientY: () => null, onSelect: () => {}, onMove: () => {}, onTrim: () => {},
      onEnvelope: () => {}, onSlip: () => {}, onLoop: () => {}, onError: () => {}, onContextMenu: () => {} };
    const view = render(() => <>
      <AudioClipBlock {...props} clip={makeAudioClip({ file: "a.wav" })} index={0} color={accent()} pxPerBeat={scale()} />
      <AudioClipBlock {...props} clip={makeAudioClip({ file: "b.wav" })} index={1} pxPerBeat={scale()} />
    </>);
    try {
      const [ca, cb] = [...view.container.querySelectorAll("canvas")];
      await waitFor(() => expect(contexts.size).toBe(2));
      const beforeA = contexts.get(ca)!.mock.calls.length, beforeB = contexts.get(cb)!.mock.calls.length, reads = computed.mock.calls.length;
      a.resolve(payload()); await loadPeaks("a.wav");
      await waitFor(() => expect(contexts.get(ca)!.mock.calls.length).toBe(beforeA + 1));
      expect(contexts.get(cb)!.mock.calls.length).toBe(beforeB); expect(computed.mock.calls.length).toBe(reads);
      setScale(36); expect(contexts.get(cb)!.mock.calls.length).toBe(beforeB + 1); expect(computed.mock.calls.length).toBe(reads);
      setAccent("rgb(140, 150, 160)"); expect(computed.mock.calls.length).toBe(reads + 3);
      themeStore.setTheme("default/dark"); expect(computed.mock.calls.length).toBe(reads + 9);
      b.resolve(payload()); await loadPeaks("b.wav"); expect(computed.mock.calls.length).toBe(reads + 9);
    } finally { view.unmount(); themeStore.setTheme("default/light"); }
  });
  it("defers offscreen requests and releases canvas memory without losing pending peaks", async () => {
    const peaks = deferred<string>();
    invokeMock.mockImplementation((cmd: string) => cmd === "audio_clip_peaks" ? peaks.promise : makeSnapshot());
    await store.initProject();
    const draw = vi.fn();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ clearRect: vi.fn(), fillRect: vi.fn(), beginPath: vi.fn(), moveTo: vi.fn(), lineTo: vi.fn(), stroke: vi.fn(), setTransform: draw } as unknown as CanvasRenderingContext2D);
    let visibility!: (visible: boolean) => void;
    const viewport = { supported: true, observe: (_element: HTMLElement, update: (visible: boolean) => void) => { visibility = update; return () => {}; } };
    const view = render(() => <AudioClipBlock viewport={viewport} clip={makeAudioClip({ file: "far.wav" })} index={0}
      color="rgb(30, 40, 50)" height={64} pxPerBeat={24} selected={false} tooltipContent={() => null}
      beatFromClientX={() => 0} trackAtClientY={() => null} onSelect={() => {}} onMove={() => {}} onTrim={() => {}}
      onEnvelope={() => {}} onSlip={() => {}} onLoop={() => {}} onError={() => {}} onContextMenu={() => {}} />);
    try {
      const canvas = view.container.querySelector("canvas")!;
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "audio_clip_peaks")).toHaveLength(0);
      expect(canvas.width).toBe(1); expect(draw).not.toHaveBeenCalled();
      visibility(true);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "audio_clip_peaks")).toHaveLength(1);
      expect(canvas.width).toBeGreaterThan(1);
      visibility(false); const count = draw.mock.calls.length;
      expect(canvas.width).toBe(1); expect(canvas.height).toBe(1);
      peaks.resolve(payload()); await loadPeaks("far.wav");
      expect(draw.mock.calls.length).toBe(count);
      visibility(true);
      expect(draw.mock.calls.length).toBe(count + 1); expect(canvas.width).toBeGreaterThan(1);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "audio_clip_peaks")).toHaveLength(1);
      expect(peaksFor("far.wav")?.frames).toBe(256);
    } finally { view.unmount(); }
  });

});
