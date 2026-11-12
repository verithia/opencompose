/**
 * i18n coverage — every label on an audited surface comes from the locale table.
 *
 * Each case mounts a surface with the locale set to zh-Hans and asserts that the
 * rendered label equals the zh-Hans table entry and not the English one. A
 * component that keeps a hard-coded English literal fails the second assertion;
 * a component that hard-codes Chinese still fails the first, because the value
 * must match `i18n.t` for the key.
 *
 * Exact translations are never written here: every expectation is read back
 * from the table, so editing a translation cannot break this suite.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent } from "@solidjs/testing-library";
import {
  loadAuditProject,
  makeTimelineProject,
  mountSurface,
  resetSurface,
  settle,
} from "./ui-audit-support";
import { makeAudioClip, makeSnapshot, makeTrack } from "./setup";
import { i18n, type I18nKey } from "../stores/i18n";
import { store } from "../stores/project";
import type { AudioClip, ParamValue } from "../types/project";
import { ChannelStrips } from "../components/ChannelStrips";
import { Mixer } from "../components/Mixer";
import { PianoRoll } from "../components/PianoRoll";
import { Timeline } from "../components/Timeline";
import { PatternPanel } from "../components/PatternPanel";
import { DrumKitPanel } from "../components/DrumKitPanel";
import { LatencyReadout } from "../components/LatencyReadout";
import { EffectsRack } from "../components/EffectsRack";
import { DslEditor } from "../components/DslEditor";
import { PerformanceEditor } from "../components/PerformanceEditor";
import { PluginBrowser } from "../components/PluginBrowser";
import { WindowControls } from "../components/WindowControls";
import { AudioClipBlock } from "../components/AudioClipBlock";
import { PatchActionDialog } from "../components/PatchActionDialog";

const windowApi = vi.hoisted(() => ({
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn(),
  isMaximized: vi.fn(async () => false),
  onResized: vi.fn(async () => () => {}),
  startResizeDragging: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => windowApi }));

const ZH = "zh-Hans" as const;

/** The table entry for a key, with the locale left on zh-Hans. */
function zh(key: I18nKey, values?: Record<string, string | number>): string {
  i18n.setLocale(ZH);
  return i18n.t(key, values);
}

function english(key: I18nKey, values?: Record<string, string | number>): string {
  i18n.setLocale("en");
  return i18n.t(key, values);
}

/** Assert one rendered string is the table entry for `key`, not English. */
function fromTable(key: I18nKey, rendered: string | null | undefined, values?: Record<string, string | number>) {
  const en = english(key, values);
  const tr = zh(key, values);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(rendered, key).toBe(tr);
}

function textOf(element: Element | null): string {
  return (element?.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** The element whose text is `value`; null when no element carries it. */
function byText(root: ParentNode, value: string): Element | null {
  return [...root.querySelectorAll("*")].find((element) => textOf(element) === value) ?? null;
}

/** Assert the accessible name of a control is the table entry for `key`. */
function ariaFromTable(root: ParentNode, key: I18nKey, values?: Record<string, string | number>) {
  const en = english(key, values);
  const tr = zh(key, values);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(root.querySelector(`[aria-label="${en}"]`), `${key}: English label still rendered`).toBeNull();
  const element = root.querySelector(`[aria-label="${tr}"]`);
  expect(element, `${key}: no control carries ${JSON.stringify(tr)}`).not.toBeNull();
}

/** Assert a placeholder is the table entry for `key`. */
function placeholderFromTable(root: ParentNode, key: I18nKey) {
  const en = english(key);
  const tr = zh(key);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(root.querySelector(`[placeholder="${en}"]`), `${key}: English placeholder still rendered`).toBeNull();
  expect(root.querySelector(`[placeholder="${tr}"]`), `${key}: no field carries ${JSON.stringify(tr)}`).not.toBeNull();
}

/** Option label for a select value. */
function optionText(root: ParentNode, value: string): string {
  return textOf(root.querySelector(`option[value="${value}"]`));
}

beforeEach(async () => {
  i18n.setLocale("en");
  await loadAuditProject(makeTimelineProject());
  i18n.setLocale(ZH);
});

afterEach(() => {
  resetSurface();
  i18n.setLocale("en");
});

function mountPianoRoll() {
  store.setSelectedTrackIndex(0);
  store.setSelectedPatternName("hook");
  return mountSurface(() => <PianoRoll />);
}

describe("piano-roll toolbar", () => {
  it("names both tools from the table", async () => {
    const view = await mountPianoRoll();
    ariaFromTable(view.container, "pianoroll.selectTool");
    ariaFromTable(view.container, "pianoroll.drawTool");
  });

  it("titles the view with the selected pattern", async () => {
    const view = await mountPianoRoll();
    const header = textOf(view.container.querySelector(".pianoroll-toolbar span"));
    fromTable("pianoroll.patternTitle", header, { name: "hook" });
  });

  it("translates the grid label, the triplet divisions and the zoom readout", async () => {
    const view = await mountPianoRoll();
    const toolbar = view.container.querySelector(".pianoroll-toolbar")!;
    expect(byText(toolbar, `${zh("menu.grid.grid")}:`), "grid label").not.toBeNull();
    expect(byText(toolbar, "Grid:"), "English grid label still rendered").toBeNull();
    fromTable("grid.tripletEighth", optionText(toolbar, "3"));
    fromTable("grid.tripletSixteenth", optionText(toolbar, "6"));

    const zoom = textOf(toolbar.querySelector(".zoom-label"));
    const pxText = zh("timeline.pxPerBeat", { px: 987654321 });
    const marker = pxText.indexOf("987654321");
    expect(zoom.startsWith(pxText.slice(0, marker))).toBe(true);
    expect(zoom.endsWith(pxText.slice(marker + "987654321".length))).toBe(true);
    expect(zoom).not.toContain("px/beat");
  });

  it("explains the empty state when no track or pattern is open", async () => {
    store.setSelectedTrackIndex(null);
    store.setSelectedPatternName(null);
    const view = await mountSurface(() => <PianoRoll />);
    fromTable("pianoroll.emptyTitle", textOf(view.container.querySelector(".empty-state-text")));
    fromTable("pianoroll.emptyHint", textOf(view.container.querySelector(".empty-state-hint")));
  });
});

function performanceSnapshot() {
  const track = makeTrack("Perf", 0);
  track.performance = {
    events: [
      { quarter: 0, channel: 0, message: { type: "program_change", program: 5 } },
      { quarter: 4, channel: 0, message: { type: "control_change", controller: 1, value: 64 } },
    ],
  };
  return makeSnapshot({ tracks: [track] });
}

describe("timeline grid pill", () => {
  async function mountTimeline() {
    await loadAuditProject(performanceSnapshot());
    i18n.setLocale(ZH);
    return mountSurface(() => <Timeline />);
  }

  it("translates the grid pill and its triplet divisions", async () => {
    const view = await mountTimeline();
    const pill = view.container.querySelector(".timeline-grid-label")!;
    fromTable("menu.grid.grid", textOf(pill.querySelector(".zoom-label")));
    fromTable("grid.tripletEighth", optionText(pill, "3"));
  });

  it("translates the performance block label and its accessible name", async () => {
    const view = await mountTimeline();
    const block = view.container.querySelector(".performance-block")!;
    expect(block, "performance block").not.toBeNull();
    fromTable("timeline.performanceBlock", textOf(block), { count: 2 });
    ariaFromTable(view.container, "timeline.performanceBlockLabel", { name: "Perf" });
  });
});

describe("mixer strips", () => {
  it("translates the strip headers and their accessible names", async () => {
    const view = await mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />);
    const strips = view.container.querySelector(".mixer-channel-strips")!;
    fromTable("mixer.channelStrips", strips.getAttribute("aria-label"));
    fromTable("mixer.sends", textOf(strips.querySelector(".channel-strip-section")));
    const track = strips.querySelector('[data-channel-strip="track:0"]')!;
    ariaFromTable(track, "mixer.faderLabel", { name: "Piano" });
    fromTable("mixer.fader", textOf(track.querySelector("label span")));

    const bus = strips.querySelector('[data-channel-strip="bus:reverb_bus"]')!;
    fromTable("mixer.busBadge", textOf(bus.querySelector(".channel-badge")));
    fromTable("mixer.return", textOf(bus.querySelector(".channel-strip-section")));

    const master = strips.querySelector('[data-channel-strip="master"]')!;
    fromTable("mixer.masterBadge", textOf(master.querySelector(".channel-badge")));
    ariaFromTable(master, "mixer.masterFader");
  });

  it("translates the channel selector in the mixer inspector", async () => {
    const view = await mountSurface(() => <Mixer />);
    const dropdown = view.container.querySelector(".mixer-channel-select")!;
    fromTable("mixer.channel", textOf(dropdown.querySelector(".oc-dropdown-label")));
    fireEvent.click(dropdown.querySelector(".oc-dropdown-trigger")!);
    await settle(1);
    const options = [...document.querySelectorAll(".oc-dropdown-option-label")].map(textOf);
    expect(options).toContain(zh("mixer.master"));
    expect(options).toContain(zh("mixer.busOption", { name: "reverb_bus" }));
  });

  it("translates the node inspector's pan readout and effects chain", async () => {
    store.setSelectedTrackIndex(0);
    const view = await mountSurface(() => <Mixer />);
    const text = textOf(view.container);
    expect(text).toContain(zh("mixer.panReadout", { value: 0 }));
    expect(text).toContain(zh("mixer.effectsChain"));
    expect(text).not.toContain("Effects Chain");
  });
});

describe("patterns panel", () => {
  it("translates the panel title, the new-pattern field and the categories", async () => {
    const view = await mountSurface(() => <PatternPanel />);
    const panel = view.container.querySelector(".pattern-panel")!;
    fromTable("panel.patterns", textOf(panel.querySelector(".pattern-panel-header span")));
    ariaFromTable(panel, "pattern.new");
    placeholderFromTable(panel, "pattern.filterPlaceholder");
  });

  it("translates the create form's category labels and buttons", async () => {
    const view = await mountSurface(() => <PatternPanel />);
    const panel = view.container.querySelector(".pattern-panel")!;
    const add = [...panel.querySelectorAll("button")].find(
      (button) => button.getAttribute("aria-label") === zh("pattern.new"),
    )!;
    fireEvent.click(add);
    await settle(1);
    placeholderFromTable(panel, "pattern.namePlaceholder");
    fromTable("pattern.category.piano", optionText(panel, "piano"));
    fromTable("store.category.drums", optionText(panel, "drums"));
    fromTable("store.category.synth", optionText(panel, "synth"));
    expect(byText(panel, zh("pattern.create")), "Create").not.toBeNull();
    expect(byText(panel, zh("dialog.cancel")), "Cancel").not.toBeNull();
  });
});

function kitSnapshot() {
  const params: [string, ParamValue][] = [
    ["pad.kick.note", { Number: 36 }],
    ["pad.kick.gain", { Db: 0 }],
    ["pad.kick.one_shot", { Bool: true }],
    ["layer.kick.0.low", { Number: 1 }],
    ["layer.kick.0.high", { Number: 127 }],
    ["layer.kick.0.sample", { String: "" }],
  ];
  const track = { ...makeTrack("Drums", 0), instrument: { category: "drums", name: "drumkit" }, preset: "Basic Kit", phrases: [] };
  return makeSnapshot({
    presets: [{
      name: "Basic Kit", source: "builtin", plugin: "builtin.drumkit", program: null, patch: null,
      file: null, root_note: null, extends: null, params, effects: [],
    }],
    tracks: [track],
  });
}

describe("drum kit panel", () => {
  it("translates the pad inspector and the empty-sample hints", async () => {
    await loadAuditProject(kitSnapshot());
    store.setSelectedTrackIndex(0);
    i18n.setLocale(ZH);
    const view = await mountSurface(() => <DrumKitPanel />);
    const panel = view.container.querySelector(".drumkit-panel")!;
    fireEvent.click([...panel.querySelectorAll(".drum-pad-name")][0]);
    await settle(1);
    fromTable("drumkit.gain", textOf([...panel.querySelectorAll(".drum-field span")][0]));
    fromTable("routing.pan", textOf([...panel.querySelectorAll(".drum-field span")][1]));
    fromTable("drumkit.missing", textOf(panel.querySelector(".drumkit-layer-row .drumkit-muted")));
  });

  it("names an empty kit from the table", async () => {
    await loadAuditProject(makeSnapshot());
    i18n.setLocale(ZH);
    const view = await mountSurface(() => <DrumKitPanel />);
    fromTable("drumkit.noKit", textOf(view.container.querySelector(".drumkit-header strong")));
    fromTable("drumkit.addKit", textOf(view.container.querySelector(".drumkit-header .drumkit-muted")));
  });
});

describe("latency readout", () => {
  it("translates the three parts and the diagnostic line", async () => {
    const view = await mountSurface(() => (
      <LatencyReadout
        readout={{
          readout: {
            track: "a",
            reported_samples: 128,
            requested_offset_samples: 0,
            applied_offset_samples: 0,
            offset_clamped: true,
            effective_samples: 128,
            bypassed: true,
            destinations: [
              { destination: "master", reference_samples: 512, compensation_samples: 384, diagnostic_delta: 128 },
            ],
          },
          note: null,
        }}
      />
    ));
    const panel = view.container.querySelector(".latency-readout")!;
    fromTable("latency.title", textOf(panel.firstElementChild));
    const text = textOf(panel);
    expect(text).toContain(zh("latency.plugins"));
    expect(text).toContain(zh("latency.manual"));
    expect(text).toContain(zh("latency.reference"));
    expect(text).toContain(zh("latency.compensation"));
    expect(text).toContain(zh("latency.makeupDelay", { delta: 128 }));
    expect(text).toContain(zh("latency.clamped"));
    expect(text).toContain(zh("latency.bypassed"));
  });
});

describe("effects rack", () => {
  it("translates the empty chain", async () => {
    const label = zh("mixer.effectsChain");
    const view = await mountSurface(() => <EffectsRack target="track:0" effects={[]} label={label} />);
    fromTable("effects.empty", textOf(view.container.querySelector(".effects-rack-empty")));
  });
});

describe("DSL editor", () => {
  it("translates the panel title", async () => {
    const view = await mountSurface(() => <DslEditor />);
    fromTable("dsl.source", textOf(view.container.querySelector(".dsl-editor-toolbar span")));
  });
});

describe("performance editor", () => {
  it("translates the header, the column names and the pager", async () => {
    const track = makeTrack("Perf", 0);
    track.performance = { events: [{ quarter: 0, channel: 0, message: { type: "program_change", program: 5 } }] };
    const view = await mountSurface(() => (
      <PerformanceEditor track={track} trackIndex={0} onEdit={async () => undefined} />
    ));
    const editor = view.container.querySelector(".performance-editor")!;
    fromTable("performance.editorLabel", editor.getAttribute("aria-label"));
    fromTable("performance.title", textOf(editor.querySelector("header strong")), { name: "Perf" });
    fromTable("performance.subtitle", textOf(editor.querySelector("header span")), { count: 1 });
    fromTable("performance.help", textOf(editor.querySelector("p")));
    const headers = [...editor.querySelectorAll("th")].map(textOf);
    fromTable("performance.quarter", headers[1]);
    fromTable("midi.import.channel", headers[2]);
    fromTable("performance.command", headers[3]);
    fromTable("performance.data", headers[4]);
    fromTable("performance.page", textOf(editor.querySelector("footer span")), { page: 1, pages: 1 });
    fromTable("performance.previous", textOf(editor.querySelector("footer button")));
  });
});

describe("plugin browser", () => {
  it("translates the header, the search field and the filters", async () => {
    const view = await mountSurface(() => <PluginBrowser />);
    const browser = view.container.querySelector(".plugin-browser")!;
    fromTable("plugin.count", textOf(browser.querySelector(".plugin-browser-header span")), { count: 0 });
    placeholderFromTable(browser, "plugin.searchPlaceholder");
    const formats = browser.querySelector(".plugin-format-select")!;
    fromTable("midi.import.allChannels", optionText(formats, "all"));
    fromTable("plugin.filterBuiltIn", optionText(formats, "Internal"));
    expect(byText(browser, zh("plugin.noMatch")), "No plugins match filter").not.toBeNull();
  });
});

describe("audio clip block", () => {
  it("translates the clip name and the trim handles", async () => {
    const clip: AudioClip = makeAudioClip({ name: "hit", fade_in_seconds: 1.5 });
    const view = await mountSurface(() => (
      <AudioClipBlock
        clip={clip}
        index={0}
        color="#fff"
        height={48}
        pxPerBeat={24}
        selected={false}
        tooltipContent={() => null}
        beatFromClientX={() => 0}
        trackAtClientY={() => null}
        onSelect={() => {}}
        onMove={() => {}}
        onTrim={() => {}}
        onEnvelope={() => {}}
        onSlip={() => {}}
        onLoop={() => {}}
        onError={() => {}}
        onContextMenu={() => {}}
      />
    ));
    ariaFromTable(view.container, "clip.audioLabel", { name: "hit" });
    ariaFromTable(view.container, "clip.trimStart");
    ariaFromTable(view.container, "clip.trimEnd");
    ariaFromTable(view.container, "clip.gainHandle", { min: -60, max: 24 });
    ariaFromTable(view.container, "clip.fadeInHandle");
    ariaFromTable(view.container, "clip.fadeOutHandle");
    expect(textOf(view.container)).toContain(zh("clip.fadeIn", { span: "1.5s" }));
  });
});

describe("dialog and window chrome", () => {
  it("translates the patch dialog's cancel button", async () => {
    const view = await mountSurface(() => (
      <PatchActionDialog
        title="Rename patch"
        initial="Halo"
        confirmLabel="Rename"
        onConfirm={async () => null}
        onClose={() => {}}
      />
    ));
    expect(byText(view.container, zh("dialog.cancel")), "Cancel").not.toBeNull();
  });

  it("translates the window caption buttons", async () => {
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    const original = navigator.userAgent;
    Object.defineProperty(navigator, "userAgent", {
      value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
      configurable: true,
    });
    try {
      const view = await mountSurface(() => <WindowControls />);
      ariaFromTable(view.container, "window.minimize");
      ariaFromTable(view.container, "window.maximize");
      ariaFromTable(view.container, "window.close");
    } finally {
      Object.defineProperty(navigator, "userAgent", { value: original, configurable: true });
      delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    }
  });
});
