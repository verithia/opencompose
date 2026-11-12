/**
 * Effect bypass is a field of the effect, not a device parameter, so the rack
 * menu works on a device that never had one and on a hosted plugin.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent } from "@solidjs/testing-library";
import type { Effect } from "../types/project";
import { loadAuditProject, mountSurface, settle } from "./ui-audit-support";
import { expectInvoked, makeEffect, makeSnapshot } from "./setup";
import { EffectsRack } from "../components/EffectsRack";
import { DevicePanel } from "../components/DevicePanel";
import { ContextMenuLayer } from "../components/ContextMenu";
import { i18n } from "../stores/i18n";
import { FALLBACK_DEVICES, findDevice } from "../devices/catalog";

function menuItem(label: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
}

function bypassItem(): HTMLButtonElement {
  const item = menuItem(i18n.t("menu.effect.bypass"));
  if (!item) throw new Error("the effect menu has no bypass item");
  return item;
}

function bypassed(effect: Effect): Effect {
  return { ...effect, bypass: true };
}

beforeEach(async () => {
  await loadAuditProject(makeSnapshot());
});

describe("effect bypass field", () => {
  it("bypasses a device that has no bypass parameter", async () => {
    // Reverb never declared `bypass`; before the field, the menu was disabled.
    const effect = makeEffect("Reverb", [["size", { Number: 0.4 }]]);
    const view = await mountSurface(() => (
      <><EffectsRack target="track:0" effects={[effect]} label="FX" /><ContextMenuLayer /></>
    ));

    fireEvent.contextMenu(view.container.querySelector(".effect-slot")!, {
      clientX: 10,
      clientY: 10,
    });
    await settle(1);
    expect(bypassItem().getAttribute("aria-disabled")).toBeNull();
    expect(bypassItem().querySelector(".context-menu-hint")).toBeNull();

    fireEvent.click(bypassItem());
    await settle();
    expectInvoked("set_effect_bypass_owned", {
      target: "track:0",
      effectIndex: 0,
      bypass: true,
    });
  });

  it("clears the field on an effect that already carries it", async () => {
    const effect = bypassed(makeEffect("Delay", [["time", { Time: 0.25 }]]));
    const view = await mountSurface(() => (
      <><EffectsRack target="track:0" effects={[effect]} label="FX" /><ContextMenuLayer /></>
    ));

    fireEvent.contextMenu(view.container.querySelector(".effect-slot")!, {
      clientX: 10,
      clientY: 10,
    });
    await settle(1);
    expect(bypassItem().getAttribute("aria-checked")).toBe("true");

    fireEvent.click(bypassItem());
    await settle();
    expectInvoked("set_effect_bypass_owned", {
      target: "track:0",
      effectIndex: 0,
      bypass: false,
    });
  });

  it("shows the state in the device panel", async () => {
    const device = findDevice(FALLBACK_DEVICES, "reverb")!;
    const effect = bypassed(makeEffect("Reverb", []));
    const view = await mountSurface(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={effect}
        device={device}
        onClose={() => {}}
      />
    ));

    expect(view.container.querySelector(".device-panel")!.classList.contains("bypassed")).toBe(true);
    expect(view.container.querySelector("[data-device-bypassed]")!.textContent)
      .toBe(i18n.t("effects.bypassed"));
  });

  it("leaves an active device panel unmarked", async () => {
    const device = findDevice(FALLBACK_DEVICES, "reverb")!;
    const effect = makeEffect("Reverb", []);
    const view = await mountSurface(() => (
      <DevicePanel
        target="track:0"
        index={0}
        count={1}
        effect={effect}
        device={device}
        onClose={() => {}}
      />
    ));

    expect(view.container.querySelector(".device-panel")!.classList.contains("bypassed")).toBe(false);
    expect(view.container.querySelector("[data-device-bypassed]")).toBeNull();
  });

  it("keeps a plugin's parameters while it is bypassed", async () => {
    const plugin: Effect = {
      effect_type: { Plugin: "org.example.fx" },
      params: [["mix", { Percentage: 50 }]],
      bypass: true,
    };
    const view = await mountSurface(() => (
      <><EffectsRack target="track:0" effects={[plugin]} label="FX" /><ContextMenuLayer /></>
    ));

    fireEvent.click(view.container.querySelector(".effect-slot")!);
    await settle();
    expect(view.container.querySelector(".effect-param-name")?.textContent).toBe("mix");
    expect(view.container.querySelector("[data-effect-bypassed]")!.textContent)
      .toBe(i18n.t("effects.bypassed"));

    fireEvent.contextMenu(view.container.querySelector(".effect-slot")!, {
      clientX: 10,
      clientY: 10,
    });
    await settle(1);
    fireEvent.click(bypassItem());
    await settle();
    expectInvoked("set_effect_bypass_owned", {
      target: "track:0",
      effectIndex: 0,
      bypass: false,
    });
  });
});
