import { synthPointer as pointer } from "../helpers/synthRange";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createSignal } from "solid-js";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { SynthParameterButton, type SynthParameterButtonProps } from "../../components/synth/SynthParameterButton";

afterEach(cleanup);

function fixture(label = "Cutoff", dragHandlers: Pick<SynthParameterButtonProps, "onDragStart" | "onDragPosition" | "onDragCancel"> = {}) {
  const onStep = vi.fn();
  const onReset = vi.fn();
  const onClose = vi.fn();
  const [disabled, setDisabled] = createSignal(false);
  const [editable, setEditable] = createSignal(true);
  const mounted = render(() => <SynthParameterButton
    label={label} valueText="1.20 kHz" position={0.5}
    disabled={disabled()} editable={editable()}
    onStep={onStep} onReset={onReset} onClose={onClose} {...dragHandlers}
  ><input type="range" aria-label={label} /></SynthParameterButton>);
  const trigger = screen.getByRole("button", { name: label });
  const input = screen.getByLabelText(label, { selector: "input" });
  const popup = document.getElementById(trigger.getAttribute("aria-controls")!)!;
  return { trigger, input, popup, onStep, onReset, onClose, setDisabled, setEditable, unmount: mounted.unmount };
}

describe("rotary parameter interaction", () => {
  it("opens a vertical editor without editing and preserves its native input on close", async () => {
    const f = fixture();
    expect(f.popup.hidden).toBe(true);
    expect(f.trigger.getAttribute("aria-describedby")).toBeTruthy();
    fireEvent.click(f.trigger);
    expect(f.popup.hidden).toBe(false);
    await waitFor(() => expect(document.activeElement).toBe(f.input));
    expect(f.onStep).not.toHaveBeenCalled();
    fireEvent.keyDown(f.input, { key: "Escape" });
    expect(f.popup.hidden).toBe(true);
    expect(document.activeElement).toBe(f.trigger);
    expect(f.onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(f.trigger);
    expect(f.popup.querySelector("input")).toBe(f.input);
  });

  it("uses wheel direction and catalog keyboard steps while leaving shortcuts and pinch untouched", () => {
    const f = fixture();
    expect(fireEvent.wheel(f.trigger, { deltaY: -100 })).toBe(false);
    fireEvent.wheel(f.trigger, { deltaY: 12 });
    fireEvent.wheel(f.trigger, { deltaY: 0 });
    fireEvent.wheel(f.trigger, { deltaY: -10, ctrlKey: true });
    fireEvent.keyDown(f.trigger, { key: "Home" });
    fireEvent.keyDown(f.trigger, { key: "End", metaKey: true });
    expect(f.onStep.mock.calls).toEqual([["ArrowUp"], ["ArrowDown"], ["Home"]]);
  });

  it("keeps the native editor open through WebKit's temporary pointer-focus gap", () => {
    const f = fixture();
    fireEvent.click(f.trigger);
    fireEvent.focusOut(f.input, { relatedTarget: null });
    expect(f.popup.hidden).toBe(false);
    fireEvent.focusOut(f.input, { relatedTarget: document.body });
    expect(f.popup.hidden).toBe(false);
    const outside = document.createElement("button");
    document.body.append(outside);
    try {
      fireEvent.focusOut(f.input, { relatedTarget: outside });
      expect(f.popup.hidden).toBe(true);
    } finally { outside.remove(); }
  });

  it("consumes Space and Enter instead of bubbling them to transport", () => {
    const f = fixture();
    const transport = vi.fn();
    window.addEventListener("keydown", transport);
    try {
      fireEvent.keyDown(f.trigger, { key: " " });
      expect(f.popup.hidden).toBe(false);
      fireEvent.keyDown(f.trigger, { key: " ", repeat: true });
      expect(f.popup.hidden).toBe(false);
      fireEvent.keyDown(f.trigger, { key: "Enter" });
      expect(f.popup.hidden).toBe(true);
      expect(transport).not.toHaveBeenCalled();
    } finally { window.removeEventListener("keydown", transport); }
  });

  it("keeps only one popup open and lets an outside pointer retain its focus", () => {
    const first = fixture("Cutoff");
    const second = fixture("Resonance");
    fireEvent.click(first.trigger);
    fireEvent.click(second.trigger);
    expect(first.popup.hidden).toBe(true);
    expect(first.onClose).toHaveBeenCalledTimes(1);
    expect(second.popup.hidden).toBe(false);
    const outside = document.createElement("button");
    document.body.append(outside);
    try {
      outside.focus();
      fireEvent.pointerDown(outside);
      expect(second.popup.hidden).toBe(true);
      expect(document.activeElement).toBe(outside);
    } finally { outside.remove(); }
  });

  it("refuses edits in Learn mode and closes an active popup when its owner becomes busy", () => {
    const f = fixture();
    f.setEditable(false);
    fireEvent.click(f.trigger);
    fireEvent.wheel(f.trigger, { deltaY: -10 });
    fireEvent.doubleClick(f.trigger);
    expect(f.popup.hidden).toBe(true);
    expect(f.onStep).not.toHaveBeenCalled();
    expect(f.onReset).not.toHaveBeenCalled();
    f.setEditable(true);
    fireEvent.click(f.trigger);
    f.setDisabled(true);
    expect(f.popup.hidden).toBe(true);
    expect(f.onClose).toHaveBeenCalledTimes(1);
  });

  it("closes on window blur and removes the global owner on unmount", () => {
    const f = fixture();
    fireEvent.click(f.trigger);
    fireEvent(window, new Event("blur"));
    expect(f.popup.hidden).toBe(true);
    fireEvent.click(f.trigger);
    cleanup();
    expect(f.onClose).toHaveBeenCalledTimes(2);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(f.onClose).toHaveBeenCalledTimes(2);
    const next = fixture("Next");
    fireEvent.click(next.trigger);
    expect(next.popup.hidden).toBe(false);
  });

  it("places its editor inside the viewport and closes when scrolling changes the anchor", () => {
    const f = fixture();
    vi.spyOn(f.trigger, "getBoundingClientRect").mockReturnValue({
      left: window.innerWidth - 10, right: window.innerWidth, top: window.innerHeight - 20,
      bottom: window.innerHeight, width: 10, height: 20, x: 0, y: 0, toJSON() {},
    });
    fireEvent.click(f.trigger);
    expect(parseFloat(f.popup.style.left) + 64).toBeLessThanOrEqual(window.innerWidth - 6);
    expect(parseFloat(f.popup.style.top) + 132).toBeLessThanOrEqual(window.innerHeight - 6);
    fireEvent.scroll(window);
    expect(f.popup.hidden).toBe(true);
  });
});


describe("rotary pointer ownership", () => {
  it("drags outside the knob, commits before hiding feedback, and suppresses the compatibility click", () => {
    const positions = vi.fn();
    const order: string[] = [];
    let f: ReturnType<typeof fixture>;
    const parentRelease = (event: PointerEvent) => {
      if (event.pointerId !== 7) return;
      order.push(f.popup.hidden ? "already-hidden" : "commit");
    };
    f = fixture("Cutoff", {
      onDragStart: () => { window.addEventListener("pointerup", parentRelease, true); return true; },
      onDragPosition: positions,
    });
    try {
      pointer(f.trigger, "pointerdown", 7, 200);
      expect(f.popup.hidden).toBe(false);
      expect(f.popup.getAttribute("data-dragging")).toBe("true");
      expect(document.activeElement).toBe(f.trigger);
      pointer(document.body, "pointermove", 7, 120);
      expect(positions).toHaveBeenLastCalledWith(1);
      pointer(window, "pointermove", 7, 280);
      expect(positions).toHaveBeenLastCalledWith(0);
      pointer(window, "pointerup", 7, 280);
      expect(order).toEqual(["commit"]);
      expect(f.popup.hidden).toBe(true);
      expect(f.onClose).not.toHaveBeenCalled();
      fireEvent.click(f.trigger, { detail: 1 });
      expect(f.popup.hidden).toBe(true);
      fireEvent.click(f.trigger, { detail: 0 });
      expect(f.popup.hidden).toBe(false);
    } finally { window.removeEventListener("pointerup", parentRelease, true); }
  });

  it("ignores another pointer and uses the captured origin with fine adjustment", () => {
    const positions = vi.fn();
    const cancel = vi.fn();
    const f = fixture("Cutoff", { onDragStart: () => true, onDragPosition: positions, onDragCancel: cancel });
    pointer(f.trigger, "pointerdown", 11, 200);
    pointer(window, "pointermove", 12, 0);
    pointer(window, "pointerup", 12, 0);
    expect(positions).not.toHaveBeenCalled();
    expect(f.popup.hidden).toBe(false);
    pointer(window, "pointermove", 11, 120, { shiftKey: true });
    expect(positions).toHaveBeenLastCalledWith(0.55);
    pointer(window, "pointermove", 11, 104);
    expect(positions.mock.calls.at(-1)![0]).toBeCloseTo(0.65);
    pointer(window, "pointercancel", 12, 120);
    expect(cancel).not.toHaveBeenCalled();
    pointer(window, "pointercancel", 11, 120);
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(cancel.mock.calls[0][0].pointerId).toBe(11);
    expect(f.popup.hidden).toBe(true);
    pointer(window, "pointermove", 11, 0);
    expect(positions).toHaveBeenCalledTimes(2);
  });

  it("cancels its captured owner when disabled and removes drag listeners on unmount", () => {
    const cancel = vi.fn();
    const positions = vi.fn();
    const f = fixture("Cutoff", { onDragStart: () => true, onDragPosition: positions, onDragCancel: cancel });
    pointer(f.trigger, "pointerdown", 3, 200);
    f.setDisabled(true);
    expect(cancel.mock.calls[0][0].pointerId).toBe(3);
    expect(f.popup.hidden).toBe(true);
    f.setDisabled(false);
    pointer(f.trigger, "pointerdown", 4, 200);
    cleanup();
    expect(cancel.mock.calls[1][0].pointerId).toBe(4);
    pointer(window, "pointermove", 4, 100);
    expect(positions).not.toHaveBeenCalled();
    expect(cancel).toHaveBeenCalledTimes(2);
  });

  it("rejects retired callbacks even when the native pointer ID is reused", () => {
    const positions = vi.fn();
    const f = fixture("Cutoff", { onDragStart: () => true, onDragPosition: positions });
    const listeners = vi.spyOn(window, "addEventListener");
    try {
      pointer(f.trigger, "pointerdown", 5, 200);
      const oldMove = listeners.mock.calls.find(([name]) => name === "pointermove")![1] as EventListener;
      const oldUp = listeners.mock.calls.find(([name]) => name === "pointerup")![1] as EventListener;
      pointer(window, "pointerup", 5, 200);
      pointer(f.trigger, "pointerdown", 5, 200);
      const detached = document.createElement("div");
      oldMove(pointer(detached, "pointermove", 5, 100));
      oldUp(pointer(detached, "pointerup", 5, 100));
      expect(positions).not.toHaveBeenCalled();
      expect(f.popup.hidden).toBe(false);
      pointer(window, "pointermove", 5, 184);
      expect(positions.mock.calls.at(-1)![0]).toBeCloseTo(0.6);
      pointer(window, "pointerup", 5, 184);
      expect(f.popup.hidden).toBe(true);
    } finally { listeners.mockRestore(); }
  });

  it("does not admit secondary, modified, Learn, or rejected pointer edits", () => {
    const begin = vi.fn(() => false);
    const f = fixture("Cutoff", { onDragStart: begin, onDragPosition: vi.fn() });
    pointer(f.trigger, "pointerdown", 1, 200, { button: 2 });
    pointer(f.trigger, "pointerdown", 1, 200, { ctrlKey: true });
    f.setEditable(false);
    pointer(f.trigger, "pointerdown", 1, 200);
    expect(begin).not.toHaveBeenCalled();
    f.setEditable(true);
    pointer(f.trigger, "pointerdown", 1, 200);
    expect(begin).toHaveBeenCalledTimes(1);
    fireEvent.click(f.trigger, { detail: 1 });
    expect(f.popup.hidden).toBe(true);
    fireEvent.keyDown(f.trigger, { key: "Enter" });
    expect(f.popup.hidden).toBe(false);
  });
});


describe("retired rotary owners", () => {
  it("cannot reopen or mutate from retained DOM after disposal, or close the new owner's popup", () => {
    const begin = vi.fn(() => true);
    const positions = vi.fn();
    const retired = fixture("Retired", { onDragStart: begin, onDragPosition: positions });
    retired.unmount();
    const current = fixture("Current");
    fireEvent.click(current.trigger);
    // Isolate stale owner callbacks from the current popup's normal outside-click dismissal.
    current.popup.append(retired.trigger);
    const listeners = vi.spyOn(window, "addEventListener");
    try {
      fireEvent.click(retired.trigger, { detail: 0 });
      fireEvent.keyDown(retired.trigger, { key: "ArrowUp" });
      fireEvent.keyDown(retired.trigger, { key: "Enter" });
      fireEvent.doubleClick(retired.trigger);
      pointer(retired.trigger, "pointerdown", 8, 200);
      pointer(window, "pointermove", 8, 100);
      expect(begin).not.toHaveBeenCalled();
      expect(positions).not.toHaveBeenCalled();
      expect(retired.onStep).not.toHaveBeenCalled();
      expect(retired.onReset).not.toHaveBeenCalled();
      expect(listeners.mock.calls.filter(([type]) => type === "pointermove")).toHaveLength(0);
      expect(current.popup.hidden).toBe(false);
      expect(current.onClose).not.toHaveBeenCalled();
    } finally { listeners.mockRestore(); retired.trigger.remove(); }
  });

  it("cancels accepted admission once when its parent synchronously disposes the control", () => {
    const canceled = vi.fn();
    const positions = vi.fn();
    let f: ReturnType<typeof fixture>;
    f = fixture("Cutoff", {
      onDragStart: () => { f.unmount(); return true; },
      onDragPosition: positions,
      onDragCancel: canceled,
    });
    const listeners = vi.spyOn(window, "addEventListener");
    try {
      pointer(f.trigger, "pointerdown", 9, 200);
      expect(canceled).toHaveBeenCalledTimes(1);
      expect(canceled.mock.calls[0][0].pointerId).toBe(9);
      expect(listeners.mock.calls.filter(([type]) => ["pointermove", "pointerup", "pointercancel"].includes(type))).toHaveLength(0);
      pointer(window, "pointermove", 9, 100);
      pointer(window, "pointerup", 9, 100);
      expect(positions).not.toHaveBeenCalled();
      expect(canceled).toHaveBeenCalledTimes(1);
    } finally { listeners.mockRestore(); }
  });

  it.each(["busy", "learn"] as const)("retires admission when a parent synchronously switches to %s", (mode) => {
    const canceled = vi.fn();
    const positions = vi.fn();
    let f: ReturnType<typeof fixture>;
    f = fixture("Cutoff", {
      onDragStart: () => {
        if (mode === "busy") f.setDisabled(true);
        else f.setEditable(false);
        return true;
      },
      onDragPosition: positions,
      onDragCancel: canceled,
    });
    const listeners = vi.spyOn(window, "addEventListener");
    try {
      pointer(f.trigger, "pointerdown", 10, 200);
      expect(canceled).toHaveBeenCalledTimes(1);
      expect(f.popup.hidden).toBe(true);
      expect(listeners.mock.calls.filter(([type]) => type === "pointermove")).toHaveLength(0);
      pointer(window, "pointermove", 10, 100);
      pointer(window, "pointerup", 10, 100);
      expect(positions).not.toHaveBeenCalled();
      expect(canceled).toHaveBeenCalledTimes(1);
    } finally { listeners.mockRestore(); }
  });

  it("cancels direct dragging when keyboard focus leaves the knob for another control", () => {
    const canceled = vi.fn();
    const positions = vi.fn();
    const f = fixture("Cutoff", { onDragStart: () => true, onDragPosition: positions, onDragCancel: canceled });
    const outside = document.createElement("button");
    document.body.append(outside);
    try {
      pointer(f.trigger, "pointerdown", 14, 200);
      outside.focus();
      expect(f.popup.hidden).toBe(true);
      expect(canceled).toHaveBeenCalledTimes(1);
      expect(document.activeElement).toBe(outside);
      pointer(window, "pointermove", 14, 100);
      pointer(window, "pointerup", 14, 100);
      expect(positions).not.toHaveBeenCalled();
      expect(canceled).toHaveBeenCalledTimes(1);
    } finally { outside.remove(); }
  });
});
