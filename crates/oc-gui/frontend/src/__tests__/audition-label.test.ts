import { describe, it, expect } from "vitest";
import { auditionLabel, legacyAuditionText, type MediaAttachment } from "../ai/media";
import { i18n } from "../stores/i18n";
const attachment: MediaAttachment = { kind: "audio", path: `/sessions/audition-media/${"a".repeat(64)}.wav`, name: `${"a".repeat(64)}.wav`, mime: "audio/wav", bytes: 128044, duration_secs: 4, sha256: "a".repeat(64) };
describe("saved audition display", () => {
  it("uses the known range without modifying the descriptor", () => {
    const before = JSON.stringify(attachment);
    const label = auditionLabel(attachment, "You asked to hear 208–212 s: this is that render.")!;
    expect(i18n.t(label.key, label.params)).toBe("Audition 208–212 s");
    expect(JSON.stringify(attachment)).toBe(before);
  });
  it("falls back for unknown or invalid ranges and preserves picked filenames", () => {
    expect(auditionLabel(attachment, "You asked to hear 212–208 s: this is that render.")?.key).toBe("ai.media.savedAudition");
    expect(auditionLabel(attachment, "summary")?.key).toBe("ai.media.savedAudition");
    expect(auditionLabel({ ...attachment, path: "/picked/take.wav" }, "summary")).toBeNull();
  });
  it("keeps the reference and candidate labels distinct", () => {
    expect(auditionLabel(attachment, "A/B reference excerpt. Verified playback match.")?.key)
      .toBe("ai.media.savedComparisonReference");
    expect(auditionLabel(attachment, "A/B candidate excerpt. Verified playback match.")?.key)
      .toBe("ai.media.savedComparisonCandidate");
  });
  it("explains legacy data loss only in generated audition messages", () => {
    const original = "You asked to hear 0–4 s: this is that render.\n[audio clip not saved]";
    expect(legacyAuditionText(original, "Render again")).toContain("Render again");
    expect(original).toContain("[audio clip not saved]");
    expect(legacyAuditionText("User wrote [audio clip not saved]", "Render again")).toBe("User wrote [audio clip not saved]");
  });
});
