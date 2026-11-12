import { describe, expect, it } from "vitest";
import { appTitle, LOGO_MARK } from "../stores/branding";

describe("branding", () => {
  it("prefixes the window title with the product name", () => {
    expect(appTitle("Wavetable Demo — OpenSynth")).toBe(
      "OpenCompose - Wavetable Demo — OpenSynth",
    );
    expect(appTitle("  Lead  ")).toBe("OpenCompose - Lead");
    expect(appTitle(null)).toBe("OpenCompose");
    expect(appTitle("   ")).toBe("OpenCompose");
  });

  it("ships one flat mark for the toolbar", () => {
    expect(LOGO_MARK.split("?")[0]).toMatch(/\.svg$/);
  });
});
