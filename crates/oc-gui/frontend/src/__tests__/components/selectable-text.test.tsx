/**
 * Read-only text stays copyable; gesture surfaces stay locked.
 *
 * jsdom does not model `user-select` inheritance and vitest runs with
 * `css: false`, so the real stylesheets are injected here and each assertion
 * pins the declaring element plus the `.selectable` marker. WebKit inherits
 * `-webkit-user-select` into a transcript's children; jsdom computes those
 * children as "auto", hence the ancestor checks.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { invokeMock, makeSnapshot, makeTrack, openStepGroups, TRANSPORT_STOPPED, aiConfigWire } from "../setup";
import { store } from "../../stores/project";
import { AiPanel } from "../../components/AiPanel";
import { Timeline } from "../../components/Timeline";
import App from "../../App";

function stylesheet(name: string): string {
  return readFileSync(`${process.cwd()}/src/styles/daw/${name}`, "utf8");
}

let injected: HTMLStyleElement[] = [];

function injectStyles(...names: string[]) {
  const el = document.createElement("style");
  el.textContent = names.map(stylesheet).join("\n");
  document.head.append(el);
  injected.push(el);
}

function selectOf(el: Element): string {
  return getComputedStyle(el).webkitUserSelect;
}

beforeEach(() => {
  invokeMock.mockClear();
  injectStyles("base.css", "timeline.css");
});

afterEach(() => {
  for (const el of injected) el.remove();
  injected = [];
  store.setError(null);
});

describe("AI panel text", () => {
  it("keeps the transcript and tool output selectable", async () => {
    let chatCalls = 0;
    invokeMock.mockImplementation(async (cmd: string, _args?: unknown) => {
      if (cmd === "load_ai_config") return aiConfigWire();
      if (cmd === "save_ai_config") return "ok";
      if (cmd === "ai_key_status") return false;
      if (cmd === "ai_catalog") return { version: "test", providers: [] };
      if (cmd === "ai_build_context") return { dsl_context: '@project "Test Project"' };
      if (cmd === "ai_tools") {
        return [{
          name: "audio_compare",
          description: "Compare two renders",
          area: "analysis",
          inputSchema: { type: "object", properties: {}, additionalProperties: true },
        }];
      }
      if (cmd === "ai_call_tool") return { score: 0.87, loudness_delta_db: -1.2 };
      if (cmd === "ai_chat") {
        chatCalls += 1;
        if (chatCalls === 1) {
          return {
            role: "assistant",
            content: null,
            tool_calls: [{ id: "call-1", type: "function", function: { name: "audio_compare", arguments: "{}" } }],
          };
        }
        return { role: "assistant", content: "The target render is 1.2 dB quieter." };
      }
      return makeSnapshot();
    });
    await store.initProject();

    const { container } = render(() => <AiPanel />);
    // loadConfig resolves after mount and clears the unsaved-config gate.
    await vi.waitFor(() => expect(container.querySelector(".ai-save-notice")).toBeFalsy());
    fireEvent.input(container.querySelector(".ai-input") as HTMLInputElement, {
      target: { value: "Compare the last render" },
    });
    fireEvent.click(container.querySelector(".ai-send-btn") as HTMLButtonElement);

    await vi.waitFor(() => expect(container.querySelector(".ai-step-group")).toBeTruthy());
    openStepGroups(container);
    await vi.waitFor(() => expect(container.querySelector(".ai-tool-result")).toBeTruthy());
    const transcript = container.querySelector(".ai-messages") as HTMLElement;
    expect(transcript.classList.contains("selectable")).toBe(true);
    expect(selectOf(transcript)).toBe("text");

    // The structured content mounts when the row is opened.
    const details = container.querySelector(".ai-tool-result") as HTMLDetailsElement;
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    await vi.waitFor(() => expect(container.querySelector(".ai-tool-rich")).toBeTruthy());
    // The readable view is the default; the raw report stays one click away.
    const toggles = container.querySelectorAll(".ai-tool-result .ai-tool-raw-toggle");
    fireEvent.click(toggles[toggles.length - 1] as HTMLElement);
    await vi.waitFor(() => expect(container.querySelector(".ai-tool-result pre")).toBeTruthy());
    const blocks = [...container.querySelectorAll(".ai-tool-result pre")];
    const report = blocks[blocks.length - 1] as HTMLElement;
    expect(report.textContent).toContain("loudness_delta_db");
    expect(report.closest(".selectable")).toBe(transcript);
  });
});

describe("error copy", () => {
  it("keeps the status-bar error selectable", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "scan_plugins") return [];
      return makeSnapshot({ tracks: [makeTrack("Keys", 0)] });
    });
    const { container } = render(() => <App />);
    await vi.waitFor(() => expect(store.project.tracks.length).toBe(1));

    store.setError("Render failed: no space left on device");
    await vi.waitFor(() => expect(container.querySelector(".status-error")).toBeTruthy());
    const error = container.querySelector(".status-error") as HTMLElement;
    expect(error.textContent).toContain("no space left on device");
    expect(error.classList.contains("selectable")).toBe(true);
    expect(selectOf(error)).toBe("text");
  });
});

describe("timeline text", () => {
  it("keeps clip and phrase labels locked", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "scan_plugins") return [];
      return makeSnapshot({ tracks: [makeTrack("Keys", 0)] });
    });
    await store.initProject();

    const { container } = render(() => <Timeline />);
    const block = container.querySelector(".phrase-block") as HTMLElement;
    expect(block).toBeTruthy();
    expect(block.classList.contains("selectable")).toBe(false);
    expect(block.closest(".selectable")).toBeNull();
    expect(selectOf(block)).toBe("none");
  });
});

describe("surfaces styled in their own stylesheet", () => {
  /** Whole grouped rule that starts with `selector,`. */
  function ruleBody(css: string, selector: string): string {
    const literal = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const pattern = new RegExp(`(?:^|\\n)${literal},[^{}]*\\{([^}]*)\\}`);
    return pattern.exec(css)?.[0] ?? "";
  }

  it("opts preset metadata and device readouts in with the prefixed property", () => {
    // These repeat in every row, so the declaration lives with the surface
    // instead of a class on each node.
    const presets = ruleBody(stylesheet("presets.css"), ".preset-detail-name");
    expect(presets).toMatch(/-webkit-user-select:\s*text/);
    expect(presets).toMatch(/[^-]user-select:\s*text/);

    const devices = ruleBody(stylesheet("devices.css"), ".device-control-name");
    expect(devices).toMatch(/-webkit-user-select:\s*text/);
    expect(devices).toMatch(/[^-]user-select:\s*text/);
    expect(devices).toContain(".device-readout");
  });
});
