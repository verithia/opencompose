/**
 * The preset editor's effect rack end to end: Add, Remove and Bypass reach the
 * backend with a `preset:` target, and the open panel follows an effect that
 * moves instead of staying on its old index.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import {
  expectInvoked,
  invokeMock,
  makeEffect,
  makeSnapshot,
  selectPreset,
  TRANSPORT_STOPPED,
} from "../setup";
import { store } from "../../stores/project";
import { PresetEditor } from "../../components/PresetEditor";
import { EffectsRack } from "../../components/EffectsRack";
import { ContextMenuLayer } from "../../components/ContextMenu";
import { i18n } from "../../stores/i18n";
import type { Effect } from "../../types/project";

function presetSnapshot(effects: Effect[]) {
  return makeSnapshot({
    presets: [{
      name: "Halo 1",
      source: "sample",
      plugin: null,
      program: null,
      patch: null,
      file: "media/halo.wav",
      root_note: null,
      extends: null,
      params: [],
      effects,
    }],
  });
}

function menuItem(label: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
}

/** A backend that owns one preset chain and answers every mutation. */
function installBackend(initial: Effect[]) {
  let effects = initial;
  const snapshot = () => presetSnapshot(effects);
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (command) {
      case "begin_parameter_gesture": return `effect-owner-${invokeMock.mock.calls.length}`;
      case "end_owned_undo_gesture": return true;
      case "scan_plugins":
        return [];
      case "get_transport_state":
        return TRANSPORT_STOPPED();
      case "add_effect":
        effects = [...effects, makeEffect(String(typed.effectType))];
        return snapshot();
      case "remove_effect":
        effects = effects.filter((_, index) => index !== Number(typed.effectIndex));
        return snapshot();
      case "set_effect_bypass_owned": {
        const index = Number(typed.effectIndex);
        const bypass = Boolean(typed.bypass);
        effects = effects.map((effect, at) =>
          at === index ? { ...effect, bypass } : effect);
        return snapshot();
      }
      case "move_effect": {
        const from = Number(typed.effectIndex);
        const to = Number(typed.newIndex);
        const next = effects.slice();
        const [moved] = next.splice(from, 1);
        next.splice(to, 0, moved);
        effects = next;
        return snapshot();
      }
      default:
        return snapshot();
    }
  });
}

beforeEach(() => {
  i18n.setLocale("en");
});

describe("preset effect rack", () => {
  it("adds, bypasses and removes through a preset: target", async () => {
    installBackend([makeEffect("DynamicEq", [])]);
    await store.initProject();
    const view = render(() => <><PresetEditor /><ContextMenuLayer /></>);
    selectPreset(view.container, "Halo 1");
    await waitFor(() => expect(view.container.querySelector(".effects-rack")).toBeTruthy());

    // Add: the device picker inserts into the preset chain.
    const rack = () => view.container.querySelector(".effects-rack")!;
    fireEvent.click(rack().querySelector(".effects-rack-add-btn")!);
    await waitFor(() => expect(document.querySelector('[data-device-id="delay"]')).toBeTruthy());
    fireEvent.click(document.querySelector('[data-device-id="delay"]')!);
    await waitFor(() => expectInvoked("add_effect", {
      target: "preset:Halo 1",
      effectType: "delay",
    }));
    await waitFor(() => expect(rack().querySelectorAll(".effect-slot")).toHaveLength(2));

    // Bypass: the slot menu writes the effect's own field.
    fireEvent.contextMenu(rack().querySelectorAll(".effect-slot")[0], { clientX: 30, clientY: 30 });
    await waitFor(() => expect(menuItem(i18n.t("menu.effect.bypass"))).toBeTruthy());
    fireEvent.click(menuItem(i18n.t("menu.effect.bypass"))!);
    await waitFor(() => expectInvoked("set_effect_bypass_owned", {
      target: "preset:Halo 1",
      effectIndex: 0,
      bypass: true,
    }));
    await waitFor(() => expect(store.project.presets[0].effects[0].bypass).toBe(true));

    // Remove: the slot's × drops it from the same chain.
    fireEvent.click(rack().querySelector(".effect-slot-remove")!);
    await waitFor(() => expectInvoked("remove_effect", {
      target: "preset:Halo 1",
      effectIndex: 0,
    }));
    await waitFor(() => expect(rack().querySelectorAll(".effect-slot")).toHaveLength(1));
  });

  it("keeps the open panel on the effect that moved", async () => {
    installBackend([makeEffect("Reverb", []), makeEffect("Delay", [])]);
    await store.initProject();
    const view = render(() => (
      <>
        <EffectsRack target="preset:Halo 1" effects={store.project.presets[0].effects} label="FX" />
        <ContextMenuLayer />
      </>
    ));
    const slots = () => view.container.querySelectorAll<HTMLElement>(".effect-slot");

    fireEvent.click(slots()[0]);
    await waitFor(() => expect(slots()[0].classList.contains("expanded")).toBe(true));
    expect(view.container.querySelector("[data-device-panel]")?.getAttribute("data-device-id"))
      .toBe("reverb");

    fireEvent.contextMenu(slots()[0], { clientX: 10, clientY: 10 });
    await waitFor(() => expect(menuItem(i18n.t("menu.effect.moveDown"))).toBeTruthy());
    fireEvent.click(menuItem(i18n.t("menu.effect.moveDown"))!);
    await waitFor(() => expectInvoked("move_effect", {
      target: "preset:Halo 1",
      effectIndex: 0,
      newIndex: 1,
    }));

    await waitFor(() => {
      expect(slots()[0].classList.contains("expanded")).toBe(false);
      expect(slots()[1].classList.contains("expanded")).toBe(true);
    });
    expect(view.container.querySelector("[data-device-panel]")?.getAttribute("data-device-id"))
      .toBe("reverb");
  });
});
