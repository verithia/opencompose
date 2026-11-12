import { chromium, expect } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { baseUrl, installTauriMock, invokeCommands, lastInvoke } from "./ui-fixture.mjs";

const outDir = path.resolve("tmp/ui-interactions");

await mkdir(outDir, { recursive: true });

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, deviceScaleFactor: 1 });
const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(error.message));
await installTauriMock(page);

async function screenshotOnFailure(name, action) {
  try {
    await action();
  } catch (error) {
    try {
      await page.screenshot({ path: path.join(outDir, `${name}-failure.png`), fullPage: true, timeout: 5_000 });
    } catch (_) {}
    throw error;
  }
}

async function expectInvoke(cmd) {
  await expect.poll(async () => (await invokeCommands(page)).includes(cmd)).toBe(true);
}

async function expectAnyInvoke(commands) {
  await expect.poll(async () => {
    const invoked = await invokeCommands(page);
    return commands.some((cmd) => invoked.includes(cmd));
  }).toBe(true);
}

async function commandSeen(commands, timeoutMs = 1500) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const invoked = await invokeCommands(page);
    if (commands.some((cmd) => invoked.includes(cmd))) return true;
    await page.waitForTimeout(50);
  }
  return false;
}

async function noteCount() {
  return page.locator(".pianoroll-note:not(.note-preview):not(.note-ghost)").count();
}

async function firstNote() {
  const note = page.locator(".pianoroll-note:not(.note-preview):not(.note-ghost)").first();
  await note.scrollIntoViewIfNeeded();
  return note;
}

async function noteAt(index) {
  const note = page.locator(".pianoroll-note:not(.note-preview):not(.note-ghost)").nth(index);
  await note.scrollIntoViewIfNeeded();
  return note;
}

async function clickNote(locator, options = {}) {
  await locator.scrollIntoViewIfNeeded();
  const box = await locator.boundingBox();
  if (!box) throw new Error("Cannot click hidden Piano Roll note");
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  const hit = await page.evaluate(({ x, y }) => {
    const el = document.elementFromPoint(x, y);
    return { tag: el?.tagName, className: el?.className ?? "", text: el?.textContent ?? "" };
  }, { x, y });
  if (!String(hit.className).includes("pianoroll-note")) {
    throw new Error(`Piano Roll note click hit ${hit.tag}.${hit.className} text=${JSON.stringify(hit.text)}`);
  }
  for (const modifier of options.modifiers ?? []) await page.keyboard.down(modifier);
  await page.mouse.click(x, y, { button: options.button ?? "left" });
  for (const modifier of [...(options.modifiers ?? [])].reverse()) await page.keyboard.up(modifier);
}

async function drag(locator, dx, dy) {
  const box = await locator.boundingBox();
  if (!box) throw new Error(`Cannot drag hidden locator: ${locator}`);
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.waitForTimeout(30);
  await page.mouse.move(x + dx, y + dy, { steps: 8 });
  await page.mouse.up();
}

async function dragNoteRightEdge(locator, dx) {
  await locator.scrollIntoViewIfNeeded();
  const box = await locator.boundingBox();
  if (!box) throw new Error("Cannot resize hidden Piano Roll note");
  const x = box.x + box.width - 2;
  const y = box.y + box.height / 2;
  const hit = await page.evaluate(({ x, y }) => {
    const el = document.elementFromPoint(x, y);
    return { tag: el?.tagName, className: el?.className ?? "", text: el?.textContent ?? "" };
  }, { x, y });
  if (!String(hit.className).includes("pianoroll-note") && !String(hit.className).includes("note-resize-handle")) {
    throw new Error(`Piano Roll resize hit ${hit.tag}.${hit.className} text=${JSON.stringify(hit.text)}`);
  }
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.waitForTimeout(30);
  await page.mouse.move(x + dx, y, { steps: 8 });
  await page.mouse.up();
}

try {
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".daw-root")).toBeVisible();
  await expect(page.locator(".clip-block")).toHaveCount(2);
  await expect(page.locator(".clip-block .clip-accent-flag").first()).toBeVisible();
  await expect(page.locator(".clip-block .clip-thumbnail").first()).toBeVisible();
  await expect(page.locator(".phrase-note-block", { hasText: "chord_pad" })).toBeVisible();
  await expect(page.locator(".phrase-note-block", { hasText: "chord_pad" }).locator(".clip-note-thumb").first()).toBeVisible();

  await screenshotOnFailure("themes", async () => {
    await page.getByRole("button", { name: "Preferences", exact: true }).click();
    await expect(page.locator(".keymap-section", { hasText: "Keymapping" })).toBeVisible();
    await expect(page.locator(".keymap-row kbd", { hasText: /^Cmd\/Ctrl\+S$/ })).toBeVisible();

    // The family select scopes the variant select and the preview grid.
    await expect(page.locator(".theme-preview-card")).toHaveCount(2);
    await expect(page.getByRole("button", { name: "Dark", exact: true }).locator(".theme-preview-mini")).toBeVisible();
    await page.selectOption(".theme-family-select", "classic");
    await expect(page.locator(".theme-preview-card")).toHaveCount(9);
    await expect(page.getByRole("button", { name: "Prism", exact: true }).locator(".theme-preview-mini")).toBeVisible();
    await page.waitForFunction(() => document.documentElement.dataset.theme === "midnight");

    // The variant select applies within the selected family.
    await page.selectOption(".theme-variant-select", "signal");
    await page.waitForFunction(() => document.documentElement.dataset.theme === "signal");
    await page.selectOption(".theme-variant-select", "vector");
    await page.waitForFunction(() => document.documentElement.dataset.theme === "vector");

    // Switching family falls back to that family's first variant.
    await page.selectOption(".theme-family-select", "catppuccin");
    await page.waitForFunction(() => document.documentElement.dataset.theme === "catppuccin-latte");
    await page.selectOption(".theme-variant-select", "mocha");
    await page.waitForFunction(() => document.documentElement.dataset.theme === "catppuccin-mocha");

    await page.selectOption(".theme-family-select", "default");
    await page.waitForFunction(() => document.documentElement.dataset.theme === "ivory");
    await page.getByRole("button", { name: "Preferences", exact: true }).click();
    await expect(page.locator(".preferences-popover")).toBeHidden();
  });

  await screenshotOnFailure("locales", async () => {
    await page.getByRole("button", { name: "Preferences", exact: true }).click();
    await page.selectOption(".locale-select", "zh-Hans");
    await page.waitForFunction(() => document.documentElement.lang === "zh-Hans");
    await page.getByRole("button", { name: "偏好设置", exact: true }).click();
    await expect(page.locator(".preferences-popover .theme-select-label", { hasText: "语言" })).toBeVisible();
    await page.selectOption(".locale-select", "en");
    await page.waitForFunction(() => document.documentElement.lang === "en");
  });

  await screenshotOnFailure("transport-and-tempo", async () => {
    await page.getByRole("button", { name: "Play/Pause (Space)", exact: true }).click();
    await expectInvoke("transport_play");
    await expect(page.locator(".transport-btn.play")).toHaveClass(/active/);

    await page.getByRole("button", { name: "Stop (reset to beginning)", exact: true }).click();
    await expectInvoke("transport_stop");

    await page.locator(".tempo-input").fill("132");
    await page.locator(".tempo-input").press("Enter");
    await page.locator(".status-project").click();
    await expectInvoke("set_tempo");
    await expect(page.locator(".tempo-input")).toHaveValue("132");
  });

  await screenshotOnFailure("track-controls", async () => {
    await page.locator(".track-preview-btn").first().click();
    await expectInvoke("preview_track");
    await page.getByRole("button", { name: "Mute", exact: true }).first().click();
    await expectInvoke("set_track_mute");
    await expect(page.getByRole("button", { name: "Unmute", exact: true }).first()).toBeVisible();
    await page.getByRole("button", { name: "Solo", exact: true }).first().click();
    await expectInvoke("set_track_solo");
    await expect(page.getByRole("button", { name: "Unsolo", exact: true }).first()).toBeVisible();
    // The preview leaves the transport on its own pass; stop it so the next
    // step starts from a stopped arrangement.
    await page.getByRole("button", { name: "Stop (reset to beginning)", exact: true }).click();
    await expectInvoke("transport_stop");
  });

  await screenshotOnFailure("vertical-header-lane-sync", async () => {
    const lanes = page.locator(".timeline-lanes");
    const headers = page.locator(".track-headers");
    const previousStyle = await lanes.getAttribute("style");
    try {
      // flex-basis (not height) drives the scroller height in the flex column.
      await lanes.evaluate((el) => { el.style.flex = "0 0 90px"; el.style.minHeight = "0"; });
      await lanes.hover();
      await page.mouse.wheel(0, 35);
      await expect.poll(() => lanes.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
      const aligned = async () => {
        const a = await page.locator(".track-header").first().boundingBox();
        const b = await page.locator(".track-lane").first().boundingBox();
        return a && b ? Math.abs(a.y - b.y) : 999;
      };
      await expect.poll(aligned).toBeLessThan(1);
      // Headers share the scroller, so they stay aligned at the bottom too.
      await lanes.evaluate((el) => { el.scrollTop = 10000; });
      await expect.poll(aligned).toBeLessThan(1);
      await lanes.evaluate((el) => { el.scrollTop = 0; });
      await expect.poll(() => lanes.evaluate((el) => el.scrollTop)).toBe(0);
      expect(await headers.evaluate((el) => el.scrollTop)).toBe(0);
    } finally {
      await lanes.evaluate((el, style) => { el.scrollTop = 0; if (style === null) el.removeAttribute("style"); else el.setAttribute("style", style); }, previousStyle);
    }
  });

  await screenshotOnFailure("follow-and-manual-scroll", async () => {
    const lanes = page.locator(".timeline-lanes");
    const advance = async (beat) => {
      await page.evaluate((beat) => window.__TAURI_INTERNALS__.invoke("transport_seek", { beat }), beat);
      await expect.poll(async () => {
        const left = await page.locator(".playback-cursor").evaluate((el) => parseFloat(el.style.left));
        return Math.round(left / 24);
      }).toBe(beat);
    };
    await page.getByRole("button", { name: "Play/Pause (Space)", exact: true }).click();
    await advance(65);
    await expect.poll(() => lanes.evaluate((el) => el.scrollLeft)).toBeGreaterThan(0);
    await lanes.hover();
    await page.mouse.wheel(-1200, 0);
    await expect.poll(() => lanes.evaluate((el) => el.scrollLeft)).toBe(0);
    await advance(70);
    await page.waitForTimeout(150);
    expect(await lanes.evaluate((el) => el.scrollLeft)).toBe(0);
    await page.getByRole("button", { name: "Play/Pause (Space)", exact: true }).click(); // pause
    await page.keyboard.press("Space"); // fresh Play reattaches
    await expect.poll(() => lanes.evaluate((el) => el.scrollLeft)).toBeGreaterThan(0);
    // A scrollbar change (no wheel event) must detach too.
    await lanes.evaluate((el) => { el.scrollLeft = 0; });
    await page.waitForTimeout(100);
    await advance(72);
    expect(await lanes.evaluate((el) => el.scrollLeft)).toBe(0);
    await page.getByRole("button", { name: "Stop (reset to beginning)", exact: true }).click();
  });

  await screenshotOnFailure("sidebar", async () => {
    await page.getByRole("button", { name: "DSL Editor (D)", exact: true }).click();
    await expect(page.locator(".right-sidebar")).toBeVisible();
    const before = await page.locator(".right-sidebar").boundingBox();
    await drag(page.locator(".sidebar-resize-handle"), -80, 0);
    const after = await page.locator(".right-sidebar").boundingBox();
    if (!before || !after || after.width <= before.width) throw new Error("Sidebar resize drag did not increase width");
    await page.locator('.sidebar-tab[aria-label="Plugins"]').click();
    await expect(page.locator(".sidebar-tab.active")).toHaveAttribute("aria-label", "Plugins");
    await page.locator('.sidebar-tab[aria-label="Store"]').click();
    await expect(page.locator(".sidebar-tab.active")).toHaveAttribute("aria-label", "Store");
    await page.locator('.sidebar-tab[aria-label="AI"]').click();
    await expect(page.locator(".sidebar-tab.active")).toHaveAttribute("aria-label", "AI");
  });

  await screenshotOnFailure("bottom-panels", async () => {
    await page.keyboard.press("KeyC");
    await expect(page.locator(".bottom-tab.active")).toContainText("Clips");
    await page.keyboard.press("KeyR");
    await expect(page.locator(".bottom-tab.active")).toContainText("Piano Roll");
    await page.keyboard.press("KeyP");
    await expect(page.locator(".bottom-tab.active")).toContainText("Presets");
    await page.keyboard.press("KeyM");
    await expect(page.locator(".bottom-tab.active")).toContainText("Mixer");

    const topBefore = await page.locator(".split-top").boundingBox();
    await drag(page.locator(".split-divider"), 0, -90);
    const topAfter = await page.locator(".split-top").boundingBox();
    if (!topBefore || !topAfter || topAfter.height >= topBefore.height) throw new Error("Split divider drag did not resize top panel");

    await page.keyboard.press("Escape");
    await expect(page.locator(".split-bottom")).toBeHidden();
  });

  await screenshotOnFailure("timeline-clicks-drags", async () => {
    await page.locator(".clip-block").first().dblclick();
    await expect(page.locator(".bottom-tab.active")).toContainText("Detail");

    await expect(page.locator(".pianoroll-toolbar")).toContainText("piano_hook");

    await page.getByRole("button", { name: "Select tool", exact: true }).click();
    await expect(page.getByRole("button", { name: "Select tool", exact: true })).toHaveClass(/active/);
    await expect.poll(noteCount).toBeGreaterThanOrEqual(3);
    const initialNoteCount = await noteCount();

    await clickNote(await firstNote());
    await expect(page.locator(".pianoroll-note.note-selected")).toHaveCount(1);
    const secondNote = page.locator(".pianoroll-note:not(.note-preview):not(.note-ghost)").nth(1);
    await secondNote.scrollIntoViewIfNeeded();
    await secondNote.click({ modifiers: ["Shift"], force: true });
    await expect(page.locator(".pianoroll-note.note-selected")).toHaveCount(2);

    await page.locator("[data-pianoroll-grid]").evaluate((element) => element.focus());
    await page.keyboard.down("Control");
    await page.keyboard.press("d");
    await page.keyboard.up("Control");
    await expectInvoke("add_pattern_note");
    await expect.poll(noteCount).toBeGreaterThan(initialNoteCount);

    await page.keyboard.press("ArrowRight");
    await expectInvoke("move_pattern_note");

    await drag(await firstNote(), 48, 0);
    const moveClipBefore = await lastInvoke(page, "move_pattern_note");
    if (!moveClipBefore?.args || moveClipBefore.args.newStartBeat < 0) throw new Error("move_pattern_note args were not recorded correctly");

    const resizeHandle = page.locator(".pianoroll-note:not(.note-preview):not(.note-ghost) .note-resize-handle").nth(2);
    await drag(resizeHandle, 120, 0);
    const commandsAfterResize = await invokeCommands(page);
    if (!commandsAfterResize.includes("set_pattern_note_duration")) {
      throw new Error("Piano Roll note resize did not emit set_pattern_note_duration");
    }

    const contextNote = await noteAt(2);
    await contextNote.click({ button: "right" });
    await page.locator(".context-menu-item", { hasText: "127 (FFF)" }).click();
    if (!(await commandSeen(["set_pattern_note_velocity", "set_note_velocity"]))) throw new Error("Piano Roll context velocity edit did not emit note velocity IPC");
    await contextNote.click({ button: "right" });
    await page.locator(".context-menu-item", { hasText: "Mute note" }).click();
    if (!(await commandSeen(["set_pattern_note_muted", "set_note_muted"]))) throw new Error("Piano Roll context mute edit did not emit note mute IPC");
    await contextNote.click({ button: "right" });
    await page.locator(".context-menu-item", { hasText: "Grace note" }).click();
    if (!(await commandSeen(["set_pattern_note_grace", "set_note_grace"]))) throw new Error("Piano Roll context grace edit did not emit note grace IPC");

    await page.locator("[data-pianoroll-grid]").evaluate((element) => element.focus());
    await page.keyboard.down("Control");
    await page.keyboard.press("a");
    await page.keyboard.up("Control");
    await expect(page.locator(".pianoroll-note.note-selected")).toHaveCount(await noteCount());
    await page.keyboard.press("Delete");
    await expectInvoke("remove_pattern_note");
    await expect.poll(noteCount).toBe(0);

    await page.getByRole("button", { name: "Draw tool", exact: true }).click();
    const grid = page.locator(".pianoroll-grid-column");
    await grid.click({ position: { x: 80, y: 420 } });
    await expectInvoke("add_pattern_note");
    await expect.poll(noteCount).toBe(1);

    await (await firstNote()).click({ button: "right" });
    await page.locator(".context-menu-item", { hasText: "Delete note" }).click();
    await expectInvoke("remove_pattern_note");
    await expect.poll(noteCount).toBe(0);

    await drag(page.locator(".clip-block").first(), 72, 0);
    await expectInvoke("move_clip");
    const moveClip = await lastInvoke(page, "move_clip");
    if (!moveClip?.args || moveClip.args.startBar < 1) throw new Error("move_clip args were not recorded correctly");

    await page.keyboard.press("Escape");
    await page.locator(".timeline-ruler").click({ position: { x: 360, y: 12 } });
    await expectInvoke("transport_seek");

    const lane = page.locator(".track-lane").first();
    await lane.click({ button: "right", position: { x: 720, y: 36 } });
    await expect(page.locator(".context-menu-item", { hasText: "Create pattern here" })).toBeVisible();
    await page.locator(".context-menu-item", { hasText: "Create pattern here" }).click();
    await expectInvoke("add_pattern");
    await expectInvoke("add_clip");
    await expect(page.locator(".bottom-tab.active")).toContainText("Detail");
  });

  await screenshotOnFailure("add-track", async () => {
    const oldCount = await page.locator(".track-header").count();
    await page.getByRole("button", { name: "+ Add Track", exact: true }).click();
    await expectInvoke("add_track");
    await expect.poll(() => page.locator(".track-header").count()).toBe(oldCount + 1);
  });

  await screenshotOnFailure("pattern-panel-properties", async () => {
    await page.keyboard.press("KeyC");
    await expect(page.locator(".bottom-tab.active")).toContainText("Clips");

    await page.locator(".pattern-panel-header input[placeholder='Filter patterns...']").fill("drum");
    await expect(page.locator(".pattern-item")).toHaveCount(1);
    await expect(page.locator(".pattern-item")).toContainText("drum_room");
    await page.locator(".pattern-panel-header input[placeholder='Filter patterns...']").fill("");

    await page.locator(".pattern-item").first().click();
    await expect(page.locator(".pattern-item.selected")).toHaveCount(1);
    await page.locator(".pattern-properties input[type='number']").first().fill("7");
    await expectInvoke("set_pattern_transpose");
    await page.locator(".pattern-properties input[type='number']").nth(1).fill("35");
    await expectInvoke("set_pattern_swing");
    await page.locator('.pattern-properties [data-pattern-color="#54a0ff"]').click();
    await expectInvoke("set_pattern_color");

    await page.getByRole("button", { name: "New pattern", exact: true }).click();
    await page.locator(".pattern-panel-header input[placeholder='name']").fill("browser_smoke_pattern");
    await page.locator(".pattern-panel-header select").selectOption("bass");
    await page.locator(".pattern-panel-header button", { hasText: "Create" }).click();
    await expectInvoke("add_pattern");
  });

  await screenshotOnFailure("preset-editor", async () => {
    await page.keyboard.press("KeyP");
    await expect(page.locator(".bottom-tab.active")).toContainText("Presets");
    await page.locator(".preset-list-item", { hasText: "Graphite Piano" }).click();
    await expect(page.locator(".preset-list-item.selected")).toContainText("Graphite Piano");
    await page.locator(".preset-preview-btn").first().click();
    await expectInvoke("preview_preset");
    const trackCountBeforePreset = await page.locator(".track-header").count();
    await page.locator(".preset-add-track-btn").first().click();
    await expectInvoke("add_track");
    await expectInvoke("set_track_preset");
    await expect.poll(() => page.locator(".track-header").count()).toBe(trackCountBeforePreset + 1);
  });

  await screenshotOnFailure("mixer-and-effects", async () => {
    await page.keyboard.press("KeyM");
    await expect(page.locator(".bottom-tab.active")).toContainText("Mixer");
    // Preset/track additions above leave another track selected; inspect Piano Lead explicitly.
    await page.getByLabel("Mixer channel", { exact: true }).selectOption({ label: "Piano Lead" });
    await expect(page.locator(".node-mixer")).toContainText("Piano Lead");

    await drag(page.locator(".mixer-fader-container").first(), 0, -32);
    await expectInvoke("update_track_mix");

    await page.locator("svg text", { hasText: "Master" }).first().click();
    await expect(page.locator(".node-mixer")).toContainText("Master");
    await page.locator("svg text", { hasText: "Piano Lead" }).first().click();
    await expect(page.locator(".node-mixer")).toContainText("Piano Lead");

    await page.locator(".effects-rack-add-btn").first().click();
    // Built-in devices now open the shared device picker.
    await page.getByRole("option", { name: /^Reverb/ }).first().click();
    await expectInvoke("add_effect");
    const reverbSlot = page.locator(".effect-slot", { hasText: "Reverb" }).first();
    await expect(reverbSlot).toBeVisible();
    const mixKnob = page.getByRole("slider", { name: "Reverb Mix", exact: true });
    // Adding expands the new slot; click collapses, a second click re-expands.
    await expect(reverbSlot).toHaveClass(/expanded/);
    await reverbSlot.click();
    await expect(mixKnob).toHaveCount(0);
    await reverbSlot.click();
    await expect(mixKnob).toBeVisible();
    await mixKnob.focus();
    await page.keyboard.press("End");
    await expectInvoke("set_effect_param");
    await expect.poll(async () => (await lastInvoke(page, "set_effect_param"))?.args.paramName).toBe("mix");
    await page.locator(".effect-slot-remove").first().click();
    await expectInvoke("remove_effect");
  });

  await screenshotOnFailure("plugins-and-dsl", async () => {
    await page.getByRole("button", { name: "DSL Editor (D)", exact: true }).click();
    await page.locator('.sidebar-tab[aria-label="Plugins"]').click();
    await expect(page.locator(".plugin-item", { hasText: "Demo Synth" })).toBeVisible();
    await page.locator(".plugin-search-input").fill("synth");
    await expect(page.locator(".plugin-item")).toHaveCount(1);
    const trackCountBeforePlugin = await page.locator(".track-header").count();
    await page.locator(".plugin-item", { hasText: "Demo Synth" }).dblclick();
    await expectInvoke("add_track");
    await expect.poll(() => page.locator(".track-header").count()).toBe(trackCountBeforePlugin + 1);
    await page.locator(".plugin-search-input").fill("");

    await page.locator('.sidebar-tab[aria-label="DSL"]').click();
    const editor = page.locator(".dsl-textarea");
    await editor.click();
    await editor.press("End");
    await editor.press("Tab");
    await expect(editor).toHaveValue(/  $/);
    await editor.press(process.platform === "darwin" ? "Meta+Enter" : "Control+Enter");
    await expectInvoke("update_dsl");
  });

  await page.screenshot({ path: path.join(outDir, "success.png"), fullPage: true });
  expect(pageErrors).toEqual([]);
  console.log(`UI interaction smoke passed. Artifacts: ${outDir}`);
} finally {
  await browser.close();
}
