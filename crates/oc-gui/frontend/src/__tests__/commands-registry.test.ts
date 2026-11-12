/**
 * The command registry is the single table behind the palette, the keymap and
 * the document handler. These tests protect its shape.
 */
import { describe, expect, it } from "vitest";
import {
  COMMANDS,
  COMMAND_CATEGORIES,
  bindingToString,
  commandById,
  formatBinding,
  normalizeKeyName,
  parseBinding,
} from "../stores/commands";
import { i18n, type I18nKey } from "../stores/i18n";

describe("command registry", () => {
  it("has unique ids", () => {
    const ids = COMMANDS.map((command) => command.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("labels every command with a key the English dictionary carries", () => {
    const keys = new Set(i18n.keys());
    for (const command of COMMANDS) {
      expect(keys.has(command.labelKey), command.id).toBe(true);
      expect(i18n.has("en", command.labelKey as I18nKey), command.id).toBe(true);
      expect(i18n.t(command.labelKey).length, command.id).toBeGreaterThan(0);
    }
  });

  it("covers the required action groups", () => {
    expect(COMMANDS.length).toBeGreaterThanOrEqual(40);
    for (const category of COMMAND_CATEGORIES) {
      const count = COMMANDS.filter((command) => command.category === category).length;
      expect(count, category).toBeGreaterThan(0);
    }
    const ids = new Set(COMMANDS.map((command) => command.id));
    for (const required of [
      "transport.playPause",
      "transport.record",
      "transport.metronome",
      "transport.loop",
      "file.new",
      "file.open",
      "file.save",
      "file.saveAs",
      "file.exportAudio",
      "file.exportMidi",
      "edit.undo",
      "edit.redo",
      "edit.cut",
      "edit.copy",
      "edit.paste",
      "edit.delete",
      "edit.duplicate",
      "edit.split",
      "edit.selectAll",
      "view.toggleMixer",
      "view.togglePianoRoll",
      "view.toggleAi",
      "view.toggleBrowser",
      "view.toggleTimeline",
      "view.zoomIn",
      "view.zoomOut",
      "view.zoomToFit",
      "view.followPlayhead",
      "view.toggleTheme",
      "view.localeEn",
      "view.localeZhHans",
      "view.localeZhHant",
      "view.localeJa",
      "view.localeEs",
      "track.add",
      "pattern.add",
      "pattern.quantise",
      "pattern.humanise",
      "pattern.legato",
      "pattern.ramp",
      "pattern.scale",
      "pattern.compress",
      "edit.groupClips",
      "edit.ungroupClips",
      "help.manual",
      "help.diagnostics",
      "help.shortcuts",
    ]) {
      expect(ids.has(required), required).toBe(true);
    }
  });

  it("never gives two commands the same default binding", () => {
    const seen = new Map<string, string>();
    for (const command of COMMANDS) {
      if (!command.defaultBinding) continue;
      const key = bindingToString(command.defaultBinding);
      expect(seen.has(key), `${key} used by ${seen.get(key)} and ${command.id}`).toBe(false);
      seen.set(key, command.id);
    }
  });
});

describe("binding descriptors", () => {
  it("round-trips through the stored string form", () => {
    const binding = parseBinding("Mod+Shift+Z");
    expect(binding).toEqual({ key: "z", mod: true, shift: true, alt: false });
    expect(bindingToString(binding!)).toBe("Mod+Shift+Z");
    expect(formatBinding(binding!)).toBe("Cmd/Ctrl+Shift+Z");
  });

  it("maps punctuation to the physical key so shifted variants agree", () => {
    expect(parseBinding("Mod+=")).toEqual({ key: "=", mod: true, shift: false, alt: false });
    expect(normalizeKeyName("+")).toBe("=");
    expect(normalizeKeyName("_")).toBe("-");
    expect(normalizeKeyName("Space")).toBe("space");
  });

  it("refuses a modifier-only binding", () => {
    expect(parseBinding("Mod")).toBeNull();
    expect(parseBinding("Shift")).toBeNull();
    expect(parseBinding("Mod+Shift")).toBeNull();
  });

  it("finds commands by id", () => {
    expect(commandById("file.save")?.category).toBe("file");
    expect(commandById("missing")).toBeUndefined();
  });
});
