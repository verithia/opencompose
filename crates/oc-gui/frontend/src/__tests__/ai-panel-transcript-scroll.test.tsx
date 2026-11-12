/**
 * The AI transcript is a native scroller with the same guards as every other
 * scroller in the GUI (`.timeline-lanes`, the piano roll, the dropdown menu):
 * no scroll anchoring, no scroll chaining, and a flex minimum that lets the
 * panel's other rows keep their height. Scrolling it must stay a browser job —
 * a passive handler reads scroll offsets only; no re-render runs per scroll event.
 *
 * vitest runs with `css: false`, so the real stylesheets are injected here and
 * the computed values are read from the declaring element, as the selectable
 * text test does for `user-select`.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";

function stylesheet(name: string): string {
  return readFileSync(`${process.cwd()}/src/styles/daw/${name}`, "utf8");
}

let injected: HTMLStyleElement[] = [];

function injectStyles(...names: string[]) {
  const element = document.createElement("style");
  element.textContent = names.map(stylesheet).join("\n");
  document.head.append(element);
  injected.push(element);
}

async function mountPanel() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") return { version: 2, active_provider: "", providers: [] };
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return { version: "test", providers: [] };
    if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    return makeSnapshot();
  });
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(view.container.querySelector(".ai-messages")).toBeTruthy());
  return view;
}

beforeEach(() => {
  invokeMock.mockReset();
  injected = [];
});

afterEach(() => {
  for (const element of injected) element.remove();
  injected = [];
});

describe("AI transcript scrolling", () => {
  it("opts out of scroll anchoring and scroll chaining like the other scrollers", async () => {
    injectStyles("base.css", "timeline.css", "panels.css");
    await mountPanel();
    const transcript = document.querySelector(".ai-messages") as HTMLElement;
    const lanes = document.createElement("div");
    lanes.className = "timeline-lanes";
    document.body.append(lanes);

    const transcriptStyle = getComputedStyle(transcript);
    const lanesStyle = getComputedStyle(lanes);
    // The guard every other scroller has: the browser never moves the offset
    // behind the user's back while content above the viewport changes.
    expect(transcriptStyle.overflowAnchor).toBe("none");
    expect(lanesStyle.overflowAnchor).toBe("none");
    // jsdom's computed style has no `overscroll-behavior`, so the chaining
    // guard is pinned in the stylesheet itself.
    const css = readFileSync(`${process.cwd()}/src/styles/daw/panels.css`, "utf8");
    const rule = css.slice(css.indexOf(".ai-messages {"), css.indexOf("}", css.indexOf(".ai-messages {")));
    expect(rule).toContain("overscroll-behavior: contain");
    // Lazy content layout changes scrollHeight while the transcript follows a
    // reply, so the panel jumps; the 300-row cap is the bound instead.
    const messageRule = css.slice(css.indexOf(".ai-message {"), css.indexOf("}", css.indexOf(".ai-message {")));
    expect(messageRule).not.toContain("content-visibility");
    // The transcript may shrink inside the panel's column; the stats line and
    // the input bar keep their height.
    expect(transcriptStyle.minHeight).toBe("0px");
    lanes.remove();
  });

  it("performs no bounding-box read and no DOM work per scroll event", async () => {
    injectStyles("base.css", "panels.css");
    const view = await mountPanel();
    const transcript = view.container.querySelector(".ai-messages") as HTMLElement;

    let rectReads = 0;
    const original = Element.prototype.getBoundingClientRect;
    Element.prototype.getBoundingClientRect = function (this: Element) {
      rectReads += 1;
      return original.call(this);
    };
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver((records) => mutations.push(...records));
    observer.observe(transcript, { subtree: true, childList: true, characterData: true, attributes: true });

    for (let index = 0; index < 20; index += 1) {
      fireEvent.scroll(transcript);
    }
    await Promise.resolve();
    observer.disconnect();
    Element.prototype.getBoundingClientRect = original;

    // Scrolling records native offsets only: no handler measures bounding boxes and nothing
    // in the transcript re-renders as a consequence of the scroll event.
    expect(rectReads).toBe(0);
    expect(mutations.length).toBe(0);
    expect(screen.queryByRole("listbox")).toBeNull();
  });
  it("keeps the composer shrinkable and long tool summaries compact without hiding expanded content", async () => {
    injectStyles("base.css", "panels.css", "ai-thinking.css");
    const view = await mountPanel();
    const input = view.container.querySelector(".ai-input")!;
    expect(getComputedStyle(input).minWidth).toBe("0px");
    expect(getComputedStyle(view.container.querySelector(".ai-messages")!).overflowX).toBe("hidden");
    expect(getComputedStyle(view.container.querySelector(".ai-attach-btn")!).flexShrink).toBe("0");
    expect(getComputedStyle(view.container.querySelector(".ai-send-btn")!).flexShrink).toBe("0");

    const tool = document.createElement("details");
    tool.className = "ai-tool-result";
    const summary = document.createElement("summary");
    const payload = JSON.stringify({ path: "very-long-project-name/".repeat(40), result: "complete" });
    summary.textContent = "render_project → " + payload;
    const content = document.createElement("pre");
    content.textContent = payload;
    tool.append(summary, content);
    view.container.querySelector(".ai-messages")!.append(tool);
    expect(getComputedStyle(summary).whiteSpace).toBe("nowrap");
    expect(getComputedStyle(summary).textOverflow).toBe("ellipsis");
    tool.open = true;
    expect(getComputedStyle(summary).whiteSpace).toBe("normal");
    expect(getComputedStyle(content).overflowWrap).toBe("anywhere");
    expect(content.textContent).toBe(payload);
  });

});
