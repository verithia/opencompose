import { describe, it, expect, afterEach, vi } from "vitest";
import { cleanup, render, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { FactoryPatchBrowser, type FactoryPatchInfo } from "../../components/FactoryPatchBrowser";
import type { AbSnapshot } from "../../stores/api";

const PATCHES: FactoryPatchInfo[] = [
  { name: "Alpha", category: "Bass" },
  { name: "Beta", category: "Pad" },
];

function matched(overrides: Partial<AbSnapshot> = {}): AbSnapshot {
  return {
    status: "matched",
    active: "b",
    a_label: "Loaded",
    b_label: "Alpha",
    a_lufs: -17.2,
    b_raw_lufs: -12.1,
    b_lufs: -17.2,
    raw_delta_lu: 5.1,
    applied_gain_db: -5.1,
    matched_delta_lu: 0.0,
    unmeasured: null,
    window_seconds: 2.0,
    ...overrides,
  };
}

function setup() {
  const [ab, setAb] = createSignal<AbSnapshot | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [auditioning, setAuditioning] = createSignal<string | null>(null);
  const onAudition = vi.fn((name: string) => setAuditioning(name));
  const onCompare = vi.fn();
  const onSwitchAb = vi.fn();
  const view = render(() => (
    <FactoryPatchBrowser
      anchor={() => document.body}
      current="Init"
      abA="Loaded"
      patches={PATCHES}
      auditioning={auditioning}
      auditionStatus={() => null}
      ab={ab}
      abBusy={busy}
      abError={error}
      onAudition={onAudition}
      onStopAudition={() => setAuditioning(null)}
      onCompare={onCompare}
      onSwitchAb={onSwitchAb}
      onLoad={() => {}}
      onClose={() => {}}
    />
  ));
  return { view, ab, setAb, busy, setBusy, error, setError, onAudition, onCompare, onSwitchAb };
}

function dialog(): HTMLElement {
  return screen.getByRole("dialog");
}

function readout(): string {
  return document.querySelector(".factory-patch-compare-readout")?.textContent?.trim() ?? "";
}

afterEach(cleanup);

describe("A/B comparison footer", () => {
  it("shows matching while background measurement is pending, then the final reason", () => {
    const { setAb } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Audition Alpha" }));
    setAb(matched({ status: "unmeasured", measurement_pending: true, unmeasured: "both" }));
    expect(readout()).toBe("Matching…");
    setAb(matched({ status: "unmeasured", measurement_pending: false, unmeasured: "both" }));
    expect(readout()).not.toBe("Matching…");
    expect(readout()).toBe("Not matched · neither patch has a measurable level");
  });

  it("starts empty, with the switch and the compare action disabled", () => {
    setup();
    expect(readout()).toBe("Audition a patch to compare it with the current one");
    expect(screen.getByRole("button", { name: /^Compare Loaded with/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Switch A/B, now B" })).toBeDisabled();
    expect(document.querySelector(".factory-patch-compare")?.textContent).toContain("—");
  });

  it("remembers the auditioned row as B and compares it with the loaded patch", () => {
    const { onAudition, onCompare, setBusy } = setup();
    fireEvent.click(screen.getByLabelText("Audition Alpha"));
    expect(onAudition).toHaveBeenCalledWith("Alpha");
    expect(readout()).toBe("Not measured");
    const compare = screen.getByRole("button", { name: "Compare Loaded with Alpha" });
    expect(compare).toBeEnabled();
    expect(compare).toHaveAttribute("title", "Compare Loaded with Alpha");
    fireEvent.click(compare);
    expect(onCompare).toHaveBeenCalledWith("Alpha");
    setBusy(true);
    expect(readout()).toBe("Matching…");
  });

  it("says matched only after a pair was measured, and switches from the button and the key", () => {
    const { setAb, onSwitchAb } = setup();
    fireEvent.click(screen.getByLabelText("Audition Alpha"));
    setAb(matched({ applied_gain_db: -5.13 }));
    expect(readout()).toBe("Matched · B -5.1 dB to A");
    const switchButton = screen.getByRole("button", { name: "Switch A/B, now B" });
    expect(switchButton).toBeEnabled();
    expect(switchButton).toHaveAttribute("title", "Switch A/B (\\ key)");
    switchButton.focus();
    expect(switchButton).toHaveFocus();
    fireEvent.click(switchButton);
    expect(onSwitchAb).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(dialog(), { key: "\\" });
    expect(onSwitchAb).toHaveBeenCalledTimes(2);
    setAb(matched({ active: "a" }));
    expect(document.querySelector(".factory-patch-compare-badge.active")?.textContent).toBe("A");
    expect(screen.getByRole("button", { name: "Switch A/B, now A" })).toBeEnabled();
  });

  it("does not carry a match over to the next candidate", () => {
    const { setAb } = setup();
    fireEvent.click(screen.getByLabelText("Audition Alpha"));
    setAb(matched());
    expect(readout()).toContain("Matched");
    fireEvent.click(screen.getByLabelText("Audition Beta"));
    expect(readout()).toBe("Not measured");
    expect(screen.getByRole("button", { name: "Switch A/B, now B" })).toBeDisabled();
  });

  it("reports an unmeasured pair and a failed compare without claiming a match", () => {
    const { setAb, setBusy, setError } = setup();
    fireEvent.click(screen.getByLabelText("Audition Alpha"));
    setAb(matched({ status: "unmeasured", applied_gain_db: null, unmeasured: "b" }));
    expect(readout()).toBe("Not matched · B has no measurable level");
    // The pair is still switchable: both takes were rendered even though one
    // had no measurable loudness.
    expect(screen.getByRole("button", { name: "Switch A/B, now B" })).toBeEnabled();
    setBusy(false);
    setError("render failed");
    setAb(null);
    expect(readout()).toBe("Match failed: render failed");
    expect(screen.getByRole("button", { name: "Switch A/B, now B" })).toBeDisabled();
  });

  it("ignores the switch key while no measured pair exists", () => {
    const { onSwitchAb } = setup();
    fireEvent.click(screen.getByLabelText("Audition Alpha"));
    fireEvent.keyDown(dialog(), { key: "\\" });
    expect(onSwitchAb).not.toHaveBeenCalled();
  });
});
