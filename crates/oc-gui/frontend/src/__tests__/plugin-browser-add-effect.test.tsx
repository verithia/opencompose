/**
 * A scanned plugin effect is inserted into the selected track from the plugin
 * browser; the row is disabled, with a reason, when no track is selected, and a
 * refused load leaves the chain untouched.
 */
import { beforeEach, expect, it } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack } from "./setup";
import type { Effect } from "../types/project";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { PluginBrowser } from "../components/PluginBrowser";

const PLUGIN = {
  id: "com.example.verb",
  name: "Example Verb",
  vendor: "Acme Audio",
  format: "Vst3",
  category: "Effect",
  path: "/plugins/Example Verb.vst3",
  num_params: 0,
  version: null,
  architectures: [],
  state_abi: null,
};

function backend(trackCount: number, refusal: string | null = null) {
  let snapshot = makeSnapshot({
    tracks: Array.from({ length: trackCount }, (_, index) =>
      makeTrack(index === 0 ? "Lead" : `Track ${index}`, index)),
  });
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "scan_plugins") return [PLUGIN];
    if (cmd === "soundfonts_list") return { dir: "/fonts", items: [] };
    if (cmd === "list_sampler_assets") return [];
    if (cmd === "project_compatibility_report") {
      return { compatible: true, installed_plugin_count: 1, resolved_plugins: [], issues: [] };
    }
    if (cmd === "add_effect") {
      if (refusal) throw new Error(refusal);
      const index = Number(String(args?.target).slice("track:".length));
      const effect: Effect = {
        effect_type: { Plugin: String(args?.effectType).replace("plugin:", "") },
        params: [],
      };
      snapshot = makeSnapshot({
        ...snapshot.project,
        tracks: snapshot.project.tracks.map((track, at) =>
          at === index ? { ...track, effects: [...track.effects, effect] } : track),
      });
    }
    return snapshot;
  });
}

const effectRow = async (view: ReturnType<typeof render>) =>
  view.findByRole("button", {
    name: i18n.t("plugin.addEffectToTrack", { name: PLUGIN.name }),
  });

beforeEach(() => invokeMock.mockClear());

it("adds a scanned effect to the selected track's FX chain", async () => {
  backend(2);
  await store.initProject();
  expect(store.selectedTrackIndex()).toBe(0);

  const view = render(() => <PluginBrowser />);
  fireEvent.click(await effectRow(view));

  await waitFor(() => {
    expect(invokeMock.mock.calls.some(([cmd, args]) =>
      cmd === "add_effect"
      && (args as Record<string, unknown>)?.target === "track:0"
      && (args as Record<string, unknown>)?.effectType === "plugin:com.example.verb")).toBe(true);
  });
  await waitFor(() => {
    expect(store.project.tracks[0]?.effects[0]?.effect_type)
      .toEqual({ Plugin: "com.example.verb" });
  });
});

it("disables the effect action and says why when no track is selected", async () => {
  backend(0);
  await store.initProject();
  expect(store.selectedTrackIndex()).toBeNull();

  const view = render(() => <PluginBrowser />);
  const add = await view.findByRole("button", {
    name: i18n.t("plugin.addEffectNoTrack", { name: PLUGIN.name }),
  }) as HTMLButtonElement;

  expect(add.disabled).toBe(true);
  fireEvent.click(add);
  expect(invokeMock.mock.calls.some(([cmd]) => cmd === "add_effect")).toBe(false);
});

it("shows the host's refusal and leaves no half-inserted effect", async () => {
  backend(1, "Plugin 'com.example.verb' is not loadable");
  await store.initProject();

  const view = render(() => <PluginBrowser />);
  fireEvent.click(await effectRow(view));

  await waitFor(() => {
    expect(view.container.textContent).toContain("is not loadable");
  });
  expect(store.project.tracks[0]?.effects).toHaveLength(0);
});
