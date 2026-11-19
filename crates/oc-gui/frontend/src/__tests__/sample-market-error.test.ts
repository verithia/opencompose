import { describe, expect, it } from "vitest";
import { SampleMarketError, sampleMarketError } from "../stores/sampleMarket";

describe("sampleMarketError", () => {
  it("keeps a refusal's code and message from the command", () => {
    const error = sampleMarketError({
      code: "licence_notice",
      message: "Licence: CC BY-NC 4.0.",
    });
    expect(error.code).toBe("licence_notice");
    expect(error.message).toBe("Licence: CC BY-NC 4.0.");
    expect(String(error)).toBe("Licence: CC BY-NC 4.0.");
  });

  it("treats a transport failure without a refusal code as no code", () => {
    expect(sampleMarketError({ code: null, message: "offline" }).code).toBeNull();
    expect(sampleMarketError("offline").code).toBeNull();
    expect(sampleMarketError("offline").message).toBe("offline");
  });

  it("returns an already normalized error unchanged", () => {
    const error = new SampleMarketError("invalid_cursor", "Start the search again.");
    expect(sampleMarketError(error)).toBe(error);
  });
});
