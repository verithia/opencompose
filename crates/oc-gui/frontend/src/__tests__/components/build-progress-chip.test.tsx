/**
 * The status chip that names a project rebuild: it reports the stage, the
 * instrument being prepared and how far the build has got, and it is absent
 * once the render thread is idle.
 */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { BuildProgressChip } from "../../components/BuildProgressChip";
import { i18n } from "../../stores/i18n";
import type { BuildSnapshot } from "../../types/transport";

afterEach(cleanup);

const BUILD: BuildSnapshot = { stage: "instruments", label: "Serum 2", done: 2, total: 22 };

describe("build progress chip", () => {
  it("names the instrument, the count and the animated sweep", () => {
    const { container } = render(() => <BuildProgressChip build={BUILD} />);
    const chip = container.querySelector(".status-chip.build-progress")!;
    expect(chip).toBeTruthy();
    expect(chip.getAttribute("role")).toBe("status");
    expect(chip.textContent).toBe("Preparing instruments · Serum 2 (3/22)");
    // The moving band is what tells the operator the build is still running.
    expect(chip.querySelector(".build-progress-sweep")).toBeTruthy();
  });

  it("reports a stage without an instrument", () => {
    const { container } = render(() => (
      <BuildProgressChip build={{ stage: "effects", label: "", done: 0, total: 0 }} />
    ));
    expect(container.querySelector(".status-chip")!.textContent).toBe("Preparing effects");
  });

  it("names the SoundFont stage and its decoded samples", () => {
    const { container } = render(() => (
      <BuildProgressChip build={{ stage: "soundfont", label: "decode", done: 37, total: 120 }} />
    ));
    expect(container.querySelector(".status-chip")!.textContent)
      .toBe("Preparing SoundFonts · Decoding samples · 37/120");
  });

  it("counts instruments loading behind playback and says when playback waits", () => {
    const loading = render(() => (
      <BuildProgressChip build={{ stage: "background", label: "", done: 12, total: 61 }} />
    ));
    expect(loading.container.querySelector(".status-chip")!.textContent)
      .toBe("Loading instruments · 12/61");
    cleanup();
    const waiting = render(() => (
      <BuildProgressChip build={{ stage: "waiting", label: "", done: 12, total: 61 }} />
    ));
    expect(waiting.container.querySelector(".status-chip")!.textContent)
      .toBe("Waiting for instruments · 12/61");
  });

  it("fills a bar with the weighted progress of the whole load", () => {
    const { container } = render(() => <BuildProgressChip build={{ ...BUILD, progress: 420 }} />);
    const fill = container.querySelector<HTMLElement>(".build-progress-fill")!;
    expect(fill).toBeTruthy();
    expect(fill.getAttribute("aria-hidden")).toBe("true");
    expect(fill.style.transform).toBe("scaleX(0.42)");
    // The text still names the work; the bar only shows how far it has got.
    expect(container.querySelector(".status-chip")!.textContent).toBe("Preparing instruments · Serum 2 (3/22)");
  });

  it("shows no bar until the load knows how much work it holds", () => {
    const { container } = render(() => <BuildProgressChip build={{ ...BUILD, progress: null }} />);
    expect(container.querySelector(".build-progress-fill")).toBeNull();
    expect(container.querySelector(".build-progress-sweep")).toBeTruthy();
  });

  it("glides forward and restarts at once when a new load begins", () => {
    const [build, setBuild] = createSignal<BuildSnapshot>({ ...BUILD, progress: 300 });
    const { container } = render(() => <BuildProgressChip build={build()} />);
    const fill = () => container.querySelector<HTMLElement>(".build-progress-fill")!;
    setBuild({ ...BUILD, progress: 650 });
    expect(fill().style.transform).toBe("scaleX(0.65)");
    expect(fill().classList.contains("restart")).toBe(false);
    setBuild({ stage: "background", label: "", done: 1, total: 74, progress: 13 });
    expect(fill().style.transform).toBe("scaleX(0.013)");
    expect(fill().classList.contains("restart")).toBe(true);
    setBuild({ stage: "background", label: "", done: 2, total: 74, progress: 27 });
    expect(fill().classList.contains("restart")).toBe(false);
  });

  it("renders nothing for an idle or absent build", () => {
    const idle = render(() => <BuildProgressChip build={{ stage: "idle", label: "", done: 0, total: 0 }} />);
    expect(idle.container.querySelector(".status-chip")).toBeNull();
    cleanup();
    const absent = render(() => <BuildProgressChip build={null} />);
    expect(absent.container.querySelector(".status-chip")).toBeNull();
  });

  it("uses the panel's language", () => {
    i18n.setLocale("zh-Hans");
    const { container } = render(() => <BuildProgressChip build={BUILD} />);
    expect(container.querySelector(".status-chip")!.textContent).toBe("准备乐器 · Serum 2（3/22）");
    i18n.setLocale("en");
  });
});
