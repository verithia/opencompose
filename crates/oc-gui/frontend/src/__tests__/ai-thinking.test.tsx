/**
 * The waiting row of a live turn. The phase is derived from the panel's own
 * facts, the seconds are a component-local clock, and the motion is decided by
 * a stylesheet class rather than by whatever the environment happens to allow.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { readFileSync } from "node:fs";
import { invokeMock, makeSnapshot, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { getActiveProvider } from "../ai/config";
import { i18n } from "../stores/i18n";
import { AiThinking, lastToolOf, thinkingPhase, type ThinkingFacts } from "../components/AiThinking";
import { AiPanel } from "../components/AiPanel";

const IDENTITY = "deepseek/deepseek-flash";

const BASE: ThinkingFacts = {
  loading: true,
  partial: "",
  approval: false,
  lastTool: null,
  identity: IDENTITY,
};

/** One live row with the facts a test steers; each prop stays reactive. */
function mountLive(facts: () => ThinkingFacts) {
  return render(() => (
    <AiThinking
      loading={facts().loading}
      partial={facts().partial}
      approval={facts().approval}
      lastTool={facts().lastTool}
      identity={facts().identity}
      retrying={facts().retrying}
    />
  ));
}

const row = () => document.querySelector(".ai-thinking") as HTMLElement | null;
const label = () => row()?.querySelector(".ai-thinking-label")?.textContent ?? "";
const seconds = () => row()?.querySelector(".ai-thinking-elapsed")?.textContent ?? "";

describe("the waiting row's phases", () => {
  it("names the provider and model before the first token", () => {
    mountLive(() => BASE);
    expect(label()).toBe(`Waiting for ${IDENTITY}`);
    expect(row()!.dataset.phase).toBe("waiting");
  });

  it("names the tool while a round runs one", () => {
    mountLive(() => ({ ...BASE, lastTool: "render_range" }));
    expect(label()).toBe("Running render_range");
    expect(row()!.dataset.phase).toBe("tool");
  });

  it("waits on the user while an approval card is open", () => {
    mountLive(() => ({ ...BASE, approval: true, lastTool: "tracks_remove" }));
    expect(label()).toBe("Waiting for your approval");
    expect(row()!.dataset.phase).toBe("approval");
  });

  it("reports a retry when the panel knows one is running", () => {
    mountLive(() => ({ ...BASE, retrying: true }));
    expect(label()).toBe("Retrying…");
    expect(row()!.dataset.phase).toBe("retry");
  });

  it("drops the label while text streams and keeps only a caret", () => {
    mountLive(() => ({ ...BASE, partial: "Adding a bass track" }));
    expect(row()!.dataset.phase).toBe("streaming");
    expect(row()!.querySelector(".ai-thinking-stream")?.textContent).toBe("Adding a bass track");
    expect(row()!.querySelector(".ai-thinking-caret")).toBeTruthy();
    expect(row()!.querySelector(".ai-thinking-label")).toBeFalsy();
    expect(row()!.querySelector(".ai-thinking-mark")).toBeFalsy();
    expect(row()!.querySelector(".ai-thinking-elapsed")).toBeFalsy();
  });

  it("renders nothing once the turn is over", () => {
    mountLive(() => ({ ...BASE, loading: false }));
    expect(row()).toBeNull();
    expect(thinkingPhase({ ...BASE, loading: false })).toBeNull();
  });
});

describe("the waiting row's clock", () => {
  // Only the clock is faked: Solid schedules its work on real microtasks.
  beforeEach(() => vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] }));
  afterEach(() => vi.useRealTimers());

  it("ticks once a second and restarts when the phase changes", async () => {
    const [facts, setFacts] = createSignal<ThinkingFacts>(BASE);
    mountLive(facts);
    await Promise.resolve();

    expect(seconds()).toBe("0s");
    await vi.advanceTimersByTimeAsync(3000);
    expect(seconds()).toBe("3s");

    setFacts({ ...BASE, lastTool: "render_range" });
    await Promise.resolve();
    expect(row()!.dataset.phase).toBe("tool");
    expect(seconds()).toBe("0s");

    await vi.advanceTimersByTimeAsync(2000);
    expect(seconds()).toBe("2s");
  });

  it("clears its one interval on unmount", async () => {
    const setIntervalSpy = vi.spyOn(globalThis, "setInterval");
    const clearIntervalSpy = vi.spyOn(globalThis, "clearInterval");
    const view = mountLive(() => BASE);
    await Promise.resolve();
    const timer = setIntervalSpy.mock.results[0]?.value as ReturnType<typeof setInterval>;
    expect(setIntervalSpy).toHaveBeenCalledTimes(1);

    view.unmount();
    expect(clearIntervalSpy).toHaveBeenCalledWith(timer);
    // Nothing may fire after the row is gone; the timers stay advanced here so
    // a leaked interval would surface as a call after unmount.
    await vi.advanceTimersByTimeAsync(5000);
    expect(clearIntervalSpy).toHaveBeenCalledTimes(1);
  });
});

describe("the waiting row's accessibility", () => {
  it("is a polite live status and keeps the seconds out of the announcement", () => {
    const view = mountLive(() => BASE);
    const status = view.getByRole("status");
    expect(status.getAttribute("aria-live")).toBe("polite");
    expect(status.getAttribute("aria-busy")).toBe("true");
    const elapsed = status.querySelector(".ai-thinking-elapsed")!;
    expect(elapsed.getAttribute("aria-hidden")).toBe("true");
    expect(elapsed.textContent).toBe("0s");
    // The label is the announced text; the number never enters it.
    expect(status.querySelector(".ai-thinking-label")!.textContent).toBe(`Waiting for ${IDENTITY}`);
  });

  it("keeps streamed tokens out of the live region", () => {
    mountLive(() => ({ ...BASE, partial: "Reading the mix" }));
    const stream = row()!.querySelector(".ai-thinking-stream")!;
    expect(stream.getAttribute("aria-live")).toBe("off");
  });
});

describe("the waiting row's motion", () => {
  const css = readFileSync(`${process.cwd()}/src/styles/daw/ai-thinking.css`, "utf8");
  const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));

  it("pulses three dots and a caret with pure CSS keyframes", () => {
    mountLive(() => BASE);
    const dots = [...row()!.querySelectorAll(".ai-thinking-dot")];
    expect(dots).toHaveLength(3);
    expect(row()!.querySelector(".ai-thinking-caret")).toBeFalsy();
    expect(css).toContain("@keyframes ai-thinking-pulse");
    expect(css).toContain("@keyframes ai-thinking-caret-blink");
    expect(css).toMatch(/\.ai-thinking-dot\s*\{[^}]*animation: ai-thinking-pulse/);
    // The row carries no inline animation: everything lives in the stylesheet.
    expect(row()!.getAttribute("style")).toBeNull();
    expect(dots.every((dot) => dot.getAttribute("style") === null)).toBe(true);
  });

  it("replaces the animation with a static mark under reduced motion", () => {
    mountLive(() => BASE);
    // The decision is made by class: the row renders both marks and the media
    // query swaps which one is shown.
    expect(row()!.querySelector(".ai-thinking-mark")).toBeTruthy();
    expect(row()!.querySelector(".ai-thinking-static")).toBeTruthy();

    expect(reduced).toContain(".ai-thinking-dot");
    expect(reduced).toContain(".ai-thinking-caret");
    expect(reduced).toMatch(/\.ai-thinking-dot,\s*\.ai-thinking-caret\s*\{[^}]*animation: none/);
    expect(reduced).toMatch(/\.ai-thinking-mark\s*\{\s*display: none;\s*\}/);
    expect(reduced).toMatch(/\.ai-thinking-static\s*\{\s*display: inline-block;\s*\}/);
    // The label itself is never animated, so it stays readable either way.
    expect(css).not.toMatch(/\.ai-thinking-label\s*\{[^}]*animation:/);
    expect(css).toMatch(/\.ai-thinking-label\s*\{[^}]*color: var\(--text-secondary\)/);
  });
});

describe("the waiting row's placement", () => {
  let scrollIntoView: unknown;

  beforeEach(() => {
    scrollIntoView = (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  });

  afterEach(() => {
    (HTMLElement.prototype as { scrollIntoView?: unknown }).scrollIntoView = scrollIntoView;
  });

  it("scrolls to itself once when it appears and never again", async () => {
    const scroll = vi.fn();
    (HTMLElement.prototype as unknown as { scrollIntoView: unknown }).scrollIntoView = scroll;
    const [facts, setFacts] = createSignal<ThinkingFacts>(BASE);
    mountLive(facts);

    await Promise.resolve();
    expect(scroll).toHaveBeenCalledTimes(1);
    expect(scroll).toHaveBeenCalledWith({ block: "nearest" });

    // A phase change updates the row in place: no second scroll, so a user who
    // scrolled away is not pulled back.
    setFacts({ ...BASE, lastTool: "render_range" });
    await Promise.resolve();
    expect(scroll).toHaveBeenCalledTimes(1);
  });
});

describe("the waiting row in the panel", () => {
  const chat = { promise: null as Promise<unknown> | null, resolve: null as ((value: unknown) => void) | null };

  function installBackend() {
    invokeMock.mockImplementation(async (cmd: string) => {
      switch (cmd) {
        case "load_ai_config": return aiConfigWire();
        case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
        case "ai_key_status": return true;
        case "ai_catalog": return { version: "test", providers: [] };
        case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
        case "ai_toolsets": return [];
        case "ai_tools": return [];
        case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
        case "ai_chat": return chat.promise ?? { role: "assistant", content: "Hello." };
        case "ai_call_tool": return { ok: true };
        default: return makeSnapshot();
      }
    });
  }

  beforeEach(() => {
    invokeMock.mockReset();
    i18n.setLocale("en");
    installBackend();
    chat.promise = new Promise((resolve) => { chat.resolve = resolve; });
  });

  afterEach(() => cleanup());

  it("shows the row after sending and drops it once the reply lands", async () => {
    await store.initProject();
    const view = render(() => <AiPanel />);
    await waitFor(() => expect(view.container.querySelector(".ai-messages")).toBeTruthy());

    const input = document.querySelector(".ai-input") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "describe the mix" } });
    fireEvent.click(document.querySelector(".ai-send-btn") as HTMLButtonElement);

    // The row appears as the turn starts, before the first token.
    await waitFor(() => expect(document.querySelector(".ai-thinking")).toBeTruthy());
    const identity = `${getActiveProvider().id}/${getActiveProvider().model}`;
    expect(document.querySelector(".ai-thinking-label")!.textContent).toBe(`Waiting for ${identity}`);
    expect(document.querySelector(".ai-thinking")!.getAttribute("role")).toBe("status");
    expect(document.querySelector(".ai-message.system")).toBeFalsy();

    chat.resolve?.({ role: "assistant", content: "Two tracks." });
    await waitFor(() => expect(document.querySelector(".ai-thinking")).toBeFalsy());
    // The row left no empty slot behind.
    expect(view.container.querySelector(".ai-messages")!.textContent).not.toContain("Waiting for");
  });
});

describe("lastToolOf", () => {
  it("names the tool of the transcript's last row only", () => {
    expect(lastToolOf([])).toBeNull();
    expect(lastToolOf([{ role: "user" }])).toBeNull();
    expect(lastToolOf([{ role: "tool", tool: { name: "render_range" } }])).toBe("render_range");
    expect(lastToolOf([
      { role: "tool", tool: { name: "render_range" } },
      { role: "assistant" },
    ])).toBeNull();
    // A text-only report keeps no name, so the row falls back to the wait line.
    expect(lastToolOf([{ role: "tool" }])).toBeNull();
  });
});
