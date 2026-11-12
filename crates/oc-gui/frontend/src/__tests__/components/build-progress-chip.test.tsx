/**
 * The status chip that names a project rebuild: it reports the stage, the
 * instrument being prepared and how far the build has got, and it is absent
 * once the render thread is idle.
 */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@solidjs/testing-library";
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
