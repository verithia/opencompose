/**
 * Control audit — mixer strips, the routing detail panel, effect racks and the
 * plugin browser.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import {
  disabledControls,
  loadAuditProject,
  mountSurface,
  resetSurface,
  settle,
  sweepSurface,
} from "./ui-audit-support";
import { invokeMock, expectInvoked, makeEffect, makeSnapshot, makeTrack } from "./setup";
import { ChannelStrips } from "../components/ChannelStrips";
import { BusActions } from "../components/BusActions";
import { Mixer } from "../components/Mixer";
import { EffectsRack } from "../components/EffectsRack";
import { ContextMenuLayer } from "../components/ContextMenu";
import { PluginBrowser } from "../components/PluginBrowser";
import { DevicePanel } from "../components/DevicePanel";
import { i18n } from "../stores/i18n";
import { findDevice, FALLBACK_DEVICES } from "../devices/catalog";

function fixture() {
  const piano = makeTrack("Piano", 0);
  piano.preset = "Halo 1";
  piano.effects = [
    makeEffect("Reverb", [["bypass", { Bool: false }], ["size", { Number: 0.4 }]]),
    makeEffect("DynamicEq", [["gain", { Number: 0 }]]),
  ];
  piano.mix.sends = [{ target: "reverb_bus", amount: 0.3 }];
  const drums = makeTrack("Drums", 1);
  return makeSnapshot({
    tracks: [piano, drums],
    buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
    master: { volume_db: 0, effects: [], input_routes: [], output_port: 0 } as never,
    presets: [
      { name: "Piano 1", source: "soundfont", plugin: null, program: 0, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] },
      { name: "Halo 1", source: "vst3", plugin: "Halo.vst3", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] },
    ],
  });
}

beforeEach(async () => {
  i18n.setLocale("en");
  await loadAuditProject(fixture());
});

/** Mutations answer with the fixture, so the surface keeps its tracks. */
function respondWithFixture(extra: (command: string) => unknown = () => undefined) {
  invokeMock.mockImplementation(async (command: string) => {
    const handled = extra(command);
    if (handled !== undefined) return handled;
    if (command === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    return fixture();
  });
}

/** The open context-menu row with this label. */
function menuItem(label: string): HTMLElement | undefined {
  return [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
}

afterEach(() => {
  resetSurface();
  i18n.setLocale("en");
});

describe("mixer sweep", () => {
  it("every control in the mixer responds or states why it cannot", async () => {
    const failures = await sweepSurface({
      mount: () => mountSurface(() => <Mixer />),
      prepare: () => loadAuditProject(fixture()),
      root: ".mixer-panel",
    });
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("every disabled mixer control carries a reason", async () => {
    await mountSurface(() => <Mixer />);
    const silent = disabledControls(document.querySelector(".mixer-panel")!).filter((c) => !c.reason);
    expect(silent, `disabled without a reason: ${silent.map((c) => c.label).join(", ")}`).toEqual([]);
  });
});

describe("channel strip sweep", () => {
  it("every strip control responds or states why it cannot", async () => {
    const failures = await sweepSurface({
      mount: () => mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />),
      prepare: () => loadAuditProject(fixture()),
      root: ".mixer-channel-strips",
    });
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("names the reason a send source cannot be chosen without a plug-in preset", async () => {
    await loadAuditProject(fixture());
    const view = await mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />);
    const select = view.container.querySelector<HTMLSelectElement>('[aria-label="Send source for Drums"]')!;
    expect(select.disabled).toBe(true);
    const reason = select.getAttribute("title") ?? "";
    expect(reason).toMatch(/plug-in|plugin/i);
  });
});

describe("channel strip actions", () => {
  it("mutes, solos, and inverts polarity on the strip it names", async () => {
    respondWithFixture();
    const view = await mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />);
    const strip = () => view.container.querySelector<HTMLElement>('[data-channel-strip="track:0"]')!;
    fireEvent.click(strip().querySelector<HTMLElement>(`[aria-label="${i18n.t("menu.track.mute")}"]`)!);
    await settle();
    expectInvoked("set_track_mute", { trackIndex: 0, mute: true });

    fireEvent.click(strip().querySelector<HTMLElement>(`[aria-label="${i18n.t("menu.track.solo")}"]`)!);
    await settle();
    expectInvoked("set_track_solo", { trackIndex: 0, solo: true });

    fireEvent.click(screen.getByRole("button", { name: "Invert polarity Piano" }));
    await settle();
    expectInvoked("set_track_channel_overrides", { trackIndex: 0, polarityInverted: true });
  });

  it("adds and removes a send", async () => {
    respondWithFixture();
    const view = await mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />);
    const target = view.container.querySelector<HTMLSelectElement>('[aria-label="Add send target for Drums"]')!;
    fireEvent.change(target, { target: { value: "reverb_bus" } });
    fireEvent.click(screen.getByRole("button", { name: "Add send from Drums" }));
    await settle();
    expectInvoked("add_send", { trackIndex: 1, busName: "reverb_bus" });

    fireEvent.click(screen.getByRole("button", { name: "Remove send to reverb_bus" }));
    await settle();
    expectInvoked("remove_send", { trackIndex: 0 });
  });

  it("moves the fader and pan it names", async () => {
    respondWithFixture();
    const view = await mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />);
    const fader = view.container.querySelector<HTMLInputElement>('[aria-label="Fader Piano"]')!;
    fireEvent.change(fader, { target: { value: "-6" } });
    await settle();
    expectInvoked("update_track_mix", { trackIndex: 0, volumeDb: -6 });
    const pan = view.container.querySelector<HTMLInputElement>('[aria-label="Pan Piano"]')!;
    fireEvent.change(pan, { target: { value: "0.5" } });
    await settle();
    expectInvoked("update_track_mix", { trackIndex: 0, pan: 0.5 });
  });

  it("renames the track through the strip's name field", async () => {
    respondWithFixture();
    const view = await mountSurface(() => <ChannelStrips selectedNode={null} onSelectNode={() => {}} />);
    const name = view.container.querySelector<HTMLInputElement>('[aria-label="Channel name for Piano"]')!;
    fireEvent.change(name, { target: { value: "Keys" } });
    await settle();
    // One name per track: the timeline and the graph follow this rename.
    expectInvoked("rename_track", { trackIndex: 0, newName: "Keys" });
  });
});

describe("bus strip", () => {
  it("asks for confirmation when a bus is still referenced and re-routes on confirm", async () => {
    respondWithFixture();
    const view = await mountSurface(() => (
      <BusActions busName="reverb_bus" onDeleted={() => {}} />
    ));
    fireEvent.click(view.container.querySelector(".routing-btn.routing-danger")!);
    await settle(1);
    const dialog = screen.getByRole("alertdialog");
    expect(dialog.textContent).toBeTruthy();
    const confirm = [...dialog.querySelectorAll("button")]
      .find((button) => button.textContent === i18n.t("routing.rerouteDelete"))!;
    fireEvent.click(confirm);
    await settle();
    expectInvoked("remove_bus", { name: "reverb_bus", reroute: true });
  });

  it("deletes an unreferenced bus directly", async () => {
    respondWithFixture();
    const opened = fixture();
    opened.project.tracks[0].mix.sends = [];
    await loadAuditProject(opened);
    const view = await mountSurface(() => (
      <BusActions busName="reverb_bus" onDeleted={() => {}} />
    ));
    fireEvent.click(view.container.querySelector(".routing-btn.routing-danger")!);
    await settle();
    expectInvoked("remove_bus", { name: "reverb_bus" });
  });
});

describe("effect rack sweep", () => {
  it("every rack control responds or states why it cannot", async () => {
    const failures = await sweepSurface({
      mount: () => mountSurface(() => (
        <EffectsRack target="track:0" effects={fixture().project.tracks[0].effects} label="FX" />
      )),
      prepare: () => loadAuditProject(fixture()),
      root: ".effects-rack",
      open: (container) => {
        // The instrument editor only exists while a slot is expanded.
        fireEvent.click(container.querySelector(".effect-slot")!);
      },
    });
    expect(failures, `controls that did nothing:\n${failures.join("\n")}`).toEqual([]);
  });

  it("adds a device from the picker and removes one from its slot", async () => {
    const view = await mountSurface(() => (
      <EffectsRack target="track:0" effects={fixture().project.tracks[0].effects} label="FX" />
    ));
    fireEvent.click(view.container.querySelector(".effects-rack-add-btn")!);
    await settle(1);
    const option = document.querySelector<HTMLElement>('[data-device-id="delay"]')!;
    expect(option).toBeTruthy();
    fireEvent.click(option);
    await settle();
    expectInvoked("add_effect", { target: "track:0", effectType: "delay" });

    fireEvent.click(view.container.querySelector('[aria-label^="Remove "]')!);
    await settle();
    expectInvoked("remove_effect", { target: "track:0", effectIndex: 0 });
  });

  it("disables the moves at the chain bounds and says why", async () => {
    const view = await mountSurface(() => (
      <><EffectsRack target="track:0" effects={[makeEffect("DynamicEq", [["gain", { Number: 0 }]])]} label="FX" /><ContextMenuLayer /></>
    ));
    fireEvent.contextMenu(view.container.querySelector(".effect-slot")!, { clientX: 10, clientY: 10 });
    await settle(1);
    expect(menuItem(i18n.t("menu.effect.moveUp"))!.getAttribute("aria-disabled")).toBe("true");
    expect(menuItem(i18n.t("menu.effect.moveUp"))!.querySelector(".context-menu-hint")?.textContent)
      .toBe(i18n.t("menu.effect.moveUpHint"));
    expect(menuItem(i18n.t("menu.effect.moveDown"))!.getAttribute("aria-disabled")).toBe("true");
    expect(menuItem(i18n.t("menu.effect.moveDown"))!.querySelector(".context-menu-hint")?.textContent)
      .toBe(i18n.t("menu.effect.moveDownHint"));
    // Bypass is a field of the effect, so it is usable even at the chain bounds.
    expect(menuItem(i18n.t("menu.effect.bypass"))!.getAttribute("aria-disabled")).toBeNull();
    expect(menuItem(i18n.t("menu.effect.bypass"))!.querySelector(".context-menu-hint")).toBeNull();
  });

  it("enables both moves inside the chain without a hint", async () => {
    const effects = [makeEffect("Reverb", []), makeEffect("Delay", []), makeEffect("Limiter", [])];
    const view = await mountSurface(() => (
      <><EffectsRack target="track:0" effects={effects} label="FX" /><ContextMenuLayer /></>
    ));
    fireEvent.contextMenu(view.container.querySelectorAll(".effect-slot")[1], { clientX: 10, clientY: 10 });
    await settle(1);
    for (const label of [i18n.t("menu.effect.moveUp"), i18n.t("menu.effect.moveDown")]) {
      expect(menuItem(label)!.getAttribute("aria-disabled")).toBeNull();
      expect(menuItem(label)!.querySelector(".context-menu-hint")).toBeNull();
    }
  });

  it("moves the effect up and down through the backend command", async () => {
    const effects = [makeEffect("Reverb", []), makeEffect("Delay", []), makeEffect("Limiter", [])];
    const view = await mountSurface(() => (
      <><EffectsRack target="track:0" effects={effects} label="FX" /><ContextMenuLayer /></>
    ));
    const middle = view.container.querySelectorAll(".effect-slot")[1];

    fireEvent.contextMenu(middle, { clientX: 10, clientY: 10 });
    await settle(1);
    fireEvent.click(menuItem(i18n.t("menu.effect.moveUp"))!);
    await settle();
    expectInvoked("move_effect", { target: "track:0", effectIndex: 1, newIndex: 0 });

    fireEvent.contextMenu(middle, { clientX: 10, clientY: 10 });
    await settle(1);
    fireEvent.click(menuItem(i18n.t("menu.effect.moveDown"))!);
    await settle();
    expectInvoked("move_effect", { target: "track:0", effectIndex: 1, newIndex: 2 });
  });

  it("reorders a preset chain through the same menu", async () => {
    const effects = [makeEffect("Reverb", []), makeEffect("Delay", [])];
    const view = await mountSurface(() => (
      <><EffectsRack target="preset:Halo 1" effects={effects} label="FX" /><ContextMenuLayer /></>
    ));
    fireEvent.contextMenu(view.container.querySelector(".effect-slot")!, { clientX: 10, clientY: 10 });
    await settle(1);
    fireEvent.click(menuItem(i18n.t("menu.effect.moveDown"))!);
    await settle();
    expectInvoked("move_effect", { target: "preset:Halo 1", effectIndex: 0, newIndex: 1 });
  });
});

describe("device panel", () => {
  it("edits a parameter with the keyboard and resets it on double click", async () => {
    const device = findDevice(FALLBACK_DEVICES, "delay")!;
    const effect = makeEffect("Delay", [["bypass", { Bool: false }], ["time", { Time: 0.25 }]]);
    const view = await mountSurface(() => (
      <DevicePanel target="track:0" index={0} count={1} effect={effect} device={device} onClose={() => {}} />
    ));
    const knob = view.container.querySelector<HTMLElement>(".device-knob")!;
    expect(knob).toBeTruthy();
    knob.focus();
    fireEvent.keyDown(knob, { key: "ArrowUp" });
    await settle();
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_effect_param_owned").length).toBeGreaterThan(0);
    invokeMock.mockClear();
    fireEvent.dblClick(knob);
    await settle();
    expectInvoked("set_effect_param_owned", { target: "track:0", effectIndex: 0 });
  });

  it("closes from its close button", async () => {
    const device = findDevice(FALLBACK_DEVICES, "reverb")!;
    let closed = false;
    const view = await mountSurface(() => (
      <DevicePanel target="track:0" index={0} count={1} effect={makeEffect("Reverb", [])} device={device} onClose={() => { closed = true; }} />
    ));
    fireEvent.click(view.container.querySelector(".device-panel-close")!);
    expect(closed).toBe(true);
  });
});

describe("plugin browser", () => {
  it("reports a failed rescan instead of silently keeping an empty list", async () => {
    await loadAuditProject(makeSnapshot());
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "scan_plugins") throw new Error("scan directory unreadable");
      if (command === "soundfonts_list") return { dir: "/tmp", items: [] };
      if (command === "list_sampler_assets") return [];
      if (command === "project_compatibility_report") {
        return { compatible: true, installed_plugin_count: 0, resolved_plugins: [], issues: [] };
      }
      return makeSnapshot();
    });
    const view = await mountSurface(() => <PluginBrowser />);
    const alert = await waitFor(() => screen.getByRole("alert"));
    expect(alert.textContent).toMatch(/scan directory unreadable/);
    expect(view.container.querySelector(".plugin-browser")).toBeTruthy();
  });

  it("adds a track from an instrument row and explains that effects have no track", async () => {
    let snapshot = makeSnapshot();
    await loadAuditProject(snapshot);
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "scan_plugins") {
        return [
          { id: "a", name: "Halo", vendor: "X", format: "Vst3", category: "Instrument", path: "/x", is_instrument: true, is_effect: false },
          { id: "b", name: "Channel EQ", vendor: "Y", format: "Vst3", category: "Effect", path: "/y", is_instrument: false, is_effect: true },
        ];
      }
      if (command === "soundfonts_list") return { dir: "/tmp", items: [] };
      if (command === "list_sampler_assets") return [];
      if (command === "project_compatibility_report") {
        return { compatible: true, installed_plugin_count: 0, resolved_plugins: [], issues: [] };
      }
      // The browser adds the preset and its track in one command.
      if (command === "add_instrument_track") snapshot = makeSnapshot({ ...snapshot.project,
        presets: [...snapshot.project.presets, { ...makeSnapshot().project.presets[0],
          name: String(args?.presetName), source: String(args?.source), plugin: String(args?.plugin) }],
        tracks: [...snapshot.project.tracks, { ...makeTrack(String(args?.trackName), 1),
          preset: String(args?.presetName) }],
      });
      return snapshot;
    });
    const view = await mountSurface(() => <PluginBrowser />);
    const rows = await waitFor(() => {
      const found = [...document.querySelectorAll<HTMLElement>(".plugin-item")];
      expect(found.length).toBe(2);
      return found;
    });
    fireEvent.dblClick(rows[0]);
    await waitFor(() => expectInvoked("add_instrument_track", { presetName: "Halo" }));
    // One command creates the preset, the track and the assignment.
    expectInvoked("add_instrument_track", {
      presetName: "Halo", source: "vst3", plugin: "a", trackName: "Halo",
    });

    invokeMock.mockClear();
    fireEvent.dblClick(rows[1]);
    await settle();
    expect(invokeMock.mock.calls.filter(([command]) => command === "add_track")).toHaveLength(0);
    expect(rows[1].getAttribute("aria-label")).toBe(i18n.t("plugin.effectRow", { name: "Channel EQ" }));
    expect(view.container.querySelector(".plugin-browser")).toBeTruthy();
  });

  it("filters the list and says when nothing matches", async () => {
    await loadAuditProject(makeSnapshot());
    const view = await mountSurface(() => <PluginBrowser />);
    const search = view.container.querySelector<HTMLInputElement>(".plugin-search-input")!;
    fireEvent.input(search, { target: { value: "zzz" } });
    await settle(1);
    expect(view.container.textContent).toContain("No plugins match filter");
  });
});
