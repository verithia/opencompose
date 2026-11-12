import { createRoot, onCleanup } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createParameterGesture } from "../utils/parameterGesture";
import {
  closeAutomationWrite,
  isAutomationWriteActive,
  resetAutomationWriteForTests,
  setAutomationWriteArmed,
  type AutomationWriteHandle,
} from "../stores/automationWrite";

function pointer(type: string, id: number): PointerEvent {
  const event = new Event(type) as PointerEvent;
  Object.defineProperties(event, {
    pointerId: { value: id }, button: { value: 0 }, timeStamp: { value: 0 },
  });
  return event;
}
afterEach(() => { vi.restoreAllMocks(); resetAutomationWriteForTests(); });

describe("parameter interaction ownership", () => {
  it("keeps captured old releases inert even with same pointer ID and zero timestamps", async () => {
    setAutomationWriteArmed(true);
    let value = 0;
    const handles: AutomationWriteHandle[] = [];
    const points: number[] = [];
    const listeners = vi.spyOn(window, "addEventListener");
    const gesture = createParameterGesture({
      value: () => value, draft: (next) => { value = next; }, clearDraft: () => {}, commit: () => {}, quarter: () => 0,
      opened: (handle) => { handles.push(handle); },
      surface: () => ({ key: "same", label: "same", value: () => value,
        position: () => ({ bar: 1, beat: 1, subdivision: 0 }), open: async () => {},
        point: async (_, next) => { points.push(next); }, close: async () => {},
      }),
    });
    gesture.pointerDown(pointer("pointerdown", 1));
    gesture.input(0.2);
    const oldRelease = listeners.mock.calls.find(([type]) => type === "pointerup")![1] as EventListener;
    const oldCancel = listeners.mock.calls.find(([type]) => type === "pointercancel")![1] as EventListener;
    oldRelease(pointer("pointerup", 1));
    await closeAutomationWrite(handles[0]);
    gesture.pointerDown(pointer("pointerdown", 1));
    gesture.input(0.8);
    oldRelease(pointer("pointerup", 1));
    oldCancel(pointer("pointercancel", 1));
    gesture.change(0.9); // Late compatibility change cannot close the active pointer session.
    expect(isAutomationWriteActive(handles[1])).toBe(true);
    expect(value).toBe(0.8);
    gesture.dispose();
    await closeAutomationWrite(handles[1]);
    expect(points).toEqual([0.2, 0.8]);
  });

  it("clears the draft when a pointer interaction is canceled", () => {
    let cleared = 0;
    const commits: number[] = [];
    const gesture = createParameterGesture({
      value: () => 0, draft: () => {}, clearDraft: () => { cleared += 1; },
      commit: next => { commits.push(next); }, surface: () => null, quarter: () => 0,
    });
    gesture.pointerDown(pointer("pointerdown", 1));
    gesture.input(0.6);
    gesture.pointerCancel(pointer("pointercancel", 1));
    expect(cleared).toBe(1);
    expect(commits).toEqual([]);
    gesture.dispose();
  });

  it("allows new explicit AX input after pointer and keyboard edits but ignores cancel's trailing change", () => {
    let value = 0;
    const commits: number[] = [];
    const gesture = createParameterGesture({
      value: () => value, draft: (next) => { value = next; }, clearDraft: () => {},
      commit: (next) => { commits.push(next); }, surface: () => null, quarter: () => 0,
    });
    gesture.pointerDown(pointer("pointerdown", 1));
    gesture.input(0.2);
    window.dispatchEvent(pointer("pointerup", 1));
    gesture.input(0.3);
    gesture.change(0.3);
    gesture.discrete(0.4);
    gesture.input(0.5);
    gesture.change(0.5);
    gesture.pointerDown(pointer("pointerdown", 1));
    gesture.input(0.6);
    gesture.pointerCancel(pointer("pointercancel", 1));
    gesture.change(0.9);
    expect(commits).toEqual([0.2, 0.3, 0.4, 0.5]);
    gesture.dispose();
  });
});


describe("ordinary parameter admission capture", () => {
  it("retains pointer-start authority through a same-type target swap before release", () => {
    let currentStamp = "original";
    let value = 0;
    const commits: string[] = [];
    const gesture = createParameterGesture({
      value: () => value, draft: next => { value = next; }, clearDraft: () => {},
      commit: () => { throw new Error("uncaptured commit is forbidden"); },
      captureCommit: () => {
        const stamp = currentStamp;
        return next => { commits.push(`${stamp}:${next}`); };
      },
      surface: () => null, quarter: () => 0,
    });
    gesture.pointerDown(pointer("pointerdown", 17));
    currentStamp = "replacement";
    gesture.input(0.8);
    window.dispatchEvent(pointer("pointerup", 17));
    expect(commits).toEqual(["original:0.8"]);
    gesture.dispose();
  });
});


describe("parameter control lifetime", () => {
  it("captures the next automation target before closing a draft retargets the control", async () => {
    setAutomationWriteArmed(true);
    let target = "original";
    let value = 0;
    const writes: string[] = [];
    const handles: AutomationWriteHandle[] = [];
    const gesture = createParameterGesture({
      value: () => value, draft: next => { value = next; },
      clearDraft: () => { target = "replacement"; }, commit: () => {}, quarter: () => 0,
      opened: handle => { handles.push(handle); },
      surface: () => {
        const captured = target;
        return { key: captured, label: captured, value: () => value,
          position: () => ({ bar: 1, beat: 1, subdivision: 0 }),
          open: async () => {}, point: async (_, next) => { writes.push(`${captured}:${next}`); },
          close: async () => {},
        };
      },
    });
    gesture.pointerDown(pointer("pointerdown", 1));
    gesture.pointerDown(pointer("pointerdown", 2));
    expect(target).toBe("replacement");
    gesture.input(0.7);
    window.dispatchEvent(pointer("pointerup", 2));
    await closeAutomationWrite(handles[1]);
    expect(writes).toEqual(["original:0.7"]);
    gesture.dispose();
  });

  it("opens an automation write when a double-click resets an armed control", async () => {
    setAutomationWriteArmed(true);
    const handles: AutomationWriteHandle[] = [];
    const writes: number[] = [];
    const gesture = createParameterGesture({
      value: () => 0.5, draft: () => {}, clearDraft: () => {}, commit: () => {}, quarter: () => 0,
      opened: handle => { handles.push(handle); },
      surface: () => ({
        key: "gain", label: "gain", value: () => 0.5,
        position: () => ({ bar: 1, beat: 1, subdivision: 0 }),
        open: async () => {},
        point: async (_, next) => { writes.push(next); },
        close: async () => {},
      }),
    });
    gesture.reset(0);
    expect(handles).toHaveLength(1);
    await closeAutomationWrite(handles[0]);
    expect(writes).toEqual([0]);
    expect(isAutomationWriteActive(handles[0])).toBe(false);
    gesture.dispose();
  });

  it("does not reopen when closing the prior draft unmounts the control", () => {
    setAutomationWriteArmed(true);
    const opened: AutomationWriteHandle[] = [];
    const closed: AutomationWriteHandle[] = [];
    const listeners = vi.spyOn(window, "addEventListener");
    let unmount = () => {};
    let shouldUnmount = false;
    const gesture = createRoot(dispose => {
      unmount = dispose;
      const owner = createParameterGesture({
        value: () => 0, draft: () => {}, commit: () => {}, quarter: () => 0,
        clearDraft: () => { if (shouldUnmount) unmount(); },
        opened: handle => { opened.push(handle); }, closed: handle => { closed.push(handle); },
        surface: () => ({ key: "gain", label: "gain", value: () => 0,
          position: () => ({ bar: 1, beat: 1, subdivision: 0 }),
          open: async () => {}, point: async () => {}, close: async () => {},
        }),
      });
      onCleanup(owner.dispose);
      return owner;
    });
    expect(gesture.pointerDown(pointer("pointerdown", 1))).toBe(true);
    const count = listeners.mock.calls.length;
    shouldUnmount = true;
    expect(gesture.pointerDown(pointer("pointerdown", 2))).toBe(false);
    expect(listeners.mock.calls).toHaveLength(count);
    expect(opened).toHaveLength(1);
    expect(closed).toEqual(opened);
    expect(isAutomationWriteActive(opened[0])).toBe(false);
    unmount();
    expect(closed).toHaveLength(1);
  });

  it("closes a just-accepted handle once when its opened callback unmounts", () => {
    setAutomationWriteArmed(true);
    const closed: AutomationWriteHandle[] = [];
    const handles: AutomationWriteHandle[] = [];
    const listeners = vi.spyOn(window, "addEventListener");
    let unmount = () => {};
    const gesture = createRoot(dispose => {
      unmount = dispose;
      const owner = createParameterGesture({
        value: () => 0, draft: () => {}, clearDraft: () => {}, commit: () => {}, quarter: () => 0,
        opened: handle => { handles.push(handle); unmount(); },
        closed: handle => { closed.push(handle); },
        surface: () => ({ key: "gain", label: "gain", value: () => 0,
          position: () => ({ bar: 1, beat: 1, subdivision: 0 }),
          open: async () => {}, point: async () => {}, close: async () => {},
        }),
      });
      onCleanup(owner.dispose);
      return owner;
    });
    expect(gesture.pointerDown(pointer("pointerdown", 1))).toBe(false);
    expect(listeners.mock.calls.filter(([type]) => type === "pointerup" || type === "pointercancel")).toEqual([]);
    expect(handles).toHaveLength(1);
    expect(closed).toEqual(handles);
    expect(isAutomationWriteActive(handles[0])).toBe(false);
    gesture.dispose();
    expect(closed).toHaveLength(1);
  });

  it("keeps retained DOM event callbacks inert after their Solid owner is disposed", () => {
    const input = document.createElement("input");
    const draft = vi.fn();
    const commit = vi.fn();
    const capture = vi.fn(() => commit);
    const surface = vi.fn(() => null);
    const listeners = vi.spyOn(window, "addEventListener");
    let unmount = () => {};
    createRoot(dispose => {
      unmount = dispose;
      const gesture = createParameterGesture({
        value: () => 0, draft, clearDraft: () => {}, commit, captureCommit: capture,
        surface, quarter: () => 0,
      });
      input.addEventListener("pointerdown", event => gesture.pointerDown(event as PointerEvent));
      input.addEventListener("input", () => gesture.input(0.5));
      input.addEventListener("change", () => gesture.change(0.5));
      input.addEventListener("keydown", () => gesture.discrete(0.8));
      input.addEventListener("dblclick", () => gesture.reset(0));
      onCleanup(gesture.dispose);
    });
    unmount();
    const count = listeners.mock.calls.length;
    input.dispatchEvent(pointer("pointerdown", 1));
    for (const event of ["input", "change", "keydown", "dblclick"]) input.dispatchEvent(new Event(event));
    expect(capture).not.toHaveBeenCalled();
    expect(surface).not.toHaveBeenCalled();
    expect(draft).not.toHaveBeenCalled();
    expect(commit).not.toHaveBeenCalled();
    expect(listeners.mock.calls).toHaveLength(count);
  });

  it("does not attach listeners after capture unmounts or commit after a draft unmounts", () => {
    const commit = vi.fn();
    const surface = vi.fn(() => null);
    const listeners = vi.spyOn(window, "addEventListener");
    let unmount = () => {};
    const capturing = createRoot(dispose => {
      unmount = dispose;
      const gesture = createParameterGesture({
        value: () => 0, draft: () => {}, clearDraft: () => {}, commit,
        captureCommit: () => { unmount(); return commit; }, surface, quarter: () => 0,
      });
      onCleanup(gesture.dispose);
      return gesture;
    });
    expect(capturing.pointerDown(pointer("pointerdown", 1))).toBe(false);
    expect(surface).not.toHaveBeenCalled();
    const drafting = createRoot(dispose => {
      const gesture = createParameterGesture({
        value: () => 0, draft: () => dispose(), clearDraft: () => {}, commit,
        surface, quarter: () => 0,
      });
      onCleanup(gesture.dispose);
      return gesture;
    });
    drafting.discrete(0.8);
    expect(commit).not.toHaveBeenCalled();
    expect(listeners.mock.calls.filter(([type]) => type === "pointerup" || type === "pointercancel")).toEqual([]);
  });
});


it("does not let an AX change close a successor opened by its draft callback", async () => {
  setAutomationWriteArmed(true);
  const handles: AutomationWriteHandle[] = [];
  let openSuccessor = true;
  const gesture = createParameterGesture({
    value: () => 0, clearDraft: () => {}, commit: () => {}, quarter: () => 0,
    draft: () => {
      if (openSuccessor) {
        openSuccessor = false;
        gesture.pointerDown(pointer("pointerdown", 7));
      }
    },
    opened: handle => { handles.push(handle); },
    surface: () => ({ key: "gain", label: "gain", value: () => 0,
      position: () => ({ bar: 1, beat: 1, subdivision: 0 }),
      open: async () => {}, point: async () => {}, close: async () => {},
    }),
  });
  gesture.change(0.5);
  expect(handles).toHaveLength(2);
  expect(isAutomationWriteActive(handles[0])).toBe(false);
  expect(isAutomationWriteActive(handles[1])).toBe(true);
  window.dispatchEvent(pointer("pointerup", 7));
  expect(isAutomationWriteActive(handles[1])).toBe(false);
  await closeAutomationWrite(handles[1]);
  gesture.dispose();
});
