/**
 * The colour row of a context menu.
 *
 * A palette shows named chips and offers the platform picker; it never asks the
 * reader to decode a hex string.
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { ContextMenuLayer, openContextMenu } from "../components/ContextMenu";

function openPalette(overrides: Record<string, unknown> = {}) {
  const onPick = vi.fn();
  const onCustom = vi.fn();
  render(() => <ContextMenuLayer />);
  openContextMenu({
    x: 10,
    y: 10,
    items: [
      {
        type: "palette",
        label: "Colour",
        colours: [
          { hex: "#6bc0e8", label: "Sky" },
          { hex: "#5cc9a0", label: "Mint" },
        ],
        value: null,
        onPick,
        onCustom,
        resetLabel: "Use theme",
        onReset: () => {},
        ...overrides,
      },
    ],
  });
  return { onPick, onCustom };
}

describe("colour palette row", () => {
  it("names every chip instead of printing its hex value", () => {
    openPalette();
    const chips = [...document.querySelectorAll<HTMLElement>(".context-menu-chip")];
    expect(chips.map((chip) => chip.getAttribute("aria-label"))).toEqual(["Sky", "Mint"]);
    expect(document.querySelector(".context-menu-palette")!.textContent).not.toContain("#");
  });

  it("reports the chip a reader picks", () => {
    const { onPick } = openPalette();
    fireEvent.click([...document.querySelectorAll<HTMLElement>(".context-menu-chip")][1]);
    expect(onPick).toHaveBeenCalledWith("#5cc9a0");
  });

  it("marks the colour in use", () => {
    openPalette({ value: "#5cc9a0" });
    const chips = [...document.querySelectorAll<HTMLElement>(".context-menu-chip")];
    expect(chips[1].classList.contains("current")).toBe(true);
    expect(chips[0].classList.contains("current")).toBe(false);
  });

  it("opens the platform picker and applies what it returns", () => {
    const { onCustom } = openPalette();
    const input = document.querySelector<HTMLInputElement>(".context-menu-palette-input")!;
    const open = vi.spyOn(input, "click");
    fireEvent.click(document.querySelector(".context-menu-palette-button")!);
    expect(open).toHaveBeenCalled();
    fireEvent.change(input, { target: { value: "#123456" } });
    expect(onCustom).toHaveBeenCalledWith("#123456");
  });

  it("keeps the reset disabled while the theme colour applies", () => {
    openPalette({ resetDisabled: true });
    const buttons = [...document.querySelectorAll<HTMLButtonElement>(".context-menu-palette-button")];
    expect(buttons[1].disabled).toBe(true);
  });
});
