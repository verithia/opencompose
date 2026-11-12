import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { ContextMenuLayer, closeContextMenu, openContextMenu } from "../../components/ContextMenu";
import { openEffectContextMenu } from "../../components/effectMenu";
import { makeEffect } from "../setup";

afterEach(() => { closeContextMenu(); cleanup(); });
async function open(onSelect: () => void = vi.fn()) {
  render(() => <><button>Opener</button><input aria-label="Next field" /><ContextMenuLayer /></>);
  const opener = screen.getByRole("button", { name: "Opener" });
  opener.focus();
  openContextMenu({ x: 20, y: 20, items: [{ label: "Action", onSelect }] });
  await waitFor(() => expect(screen.getByRole("menu")).toHaveFocus());
  return opener;
}
it("returns focus after Escape", async () => {
  const opener = await open();
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  expect(opener).toHaveFocus();
});
it("restores focus before an action opens another surface", async () => {
  let focusAtAction: Element | null = null;
  const opener = await open(() => {
    focusAtAction = document.activeElement;
    screen.getByRole("textbox").focus();
  });
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Enter" });
  expect(focusAtAction).toBe(opener);
  await Promise.resolve();
  expect(screen.getByRole("textbox")).toHaveFocus();
});
it("returns focus on Tab without consuming normal tab navigation", async () => {
  const opener = await open();
  const event = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
  screen.getByRole("menu").dispatchEvent(event);
  expect(event.defaultPrevented).toBe(false);
  expect(opener).toHaveFocus();
});
it("does not steal focus from an outside click target", async () => {
  await open();
  const field = screen.getByRole("textbox");
  field.focus();
  fireEvent.pointerDown(field);
  expect(field).toHaveFocus();
  expect(screen.queryByRole("menu")).toBeNull();
});
it("describes independent bypass as a checkbox", () => {
  render(() => <ContextMenuLayer />);
  openEffectContextMenu({ x: 20, y: 20, target: "master", index: 0, count: 1, effect: makeEffect("DynamicEq", []) });
  expect(screen.getByRole("menuitemcheckbox", { name: "Bypass" })).toHaveAttribute("aria-checked", "false");
});
it("keeps exclusive choices as radio items", () => {
  render(() => <ContextMenuLayer />);
  openContextMenu({ x: 20, y: 20, items: [{ label: "Linear", checked: true, onSelect: vi.fn() }] });
  expect(screen.getByRole("menuitemradio", { name: "Linear" })).toHaveAttribute("aria-checked", "true");
});

it("keeps an unhandled key away from the shortcuts behind the menu", async () => {
  await open();
  const behind = vi.fn();
  document.addEventListener("keydown", behind);
  try {
    const event = new KeyboardEvent("keydown", { key: "Delete", bubbles: true, cancelable: true });
    screen.getByRole("menu").dispatchEvent(event);
    // The menu stays open and the document never sees the key, so a Delete
    // cannot act on whatever was selected before the right-click.
    expect(behind).not.toHaveBeenCalled();
    expect(screen.getByRole("menu")).toBeTruthy();
  } finally {
    document.removeEventListener("keydown", behind);
  }
});
