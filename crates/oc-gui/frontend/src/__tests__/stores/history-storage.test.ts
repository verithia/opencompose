import { beforeEach, describe, expect, it } from "vitest";
import { invokeMock } from "../setup";
import { DEFAULT_NOTICE_BYTES, historyStorage } from "../../stores/historyStorage";
import { briefText, sizeText } from "../../stores/historyText";
import { rememberSessionTitles } from "../../stores/sessionTitles";

const info = (bytes: number, journaled = true) => ({ total: 3, applied: 3, journaled, bytes });

beforeEach(() => {
  invokeMock.mockReset();
  historyStorage.setNoticeBytes(DEFAULT_NOTICE_BYTES);
});

describe("the history size notice", () => {
  it("offers clearing only past the chosen size, and never for an unsaved document", async () => {
    invokeMock.mockResolvedValue(info(DEFAULT_NOTICE_BYTES + 1));
    await historyStorage.refresh();
    expect(historyStorage.overNotice()).toBe(true);
    historyStorage.setNoticeBytes(4 * 1024 ** 3);
    expect(historyStorage.overNotice()).toBe(false);
    historyStorage.setNoticeBytes(1);
    invokeMock.mockResolvedValue(info(10, false));
    await historyStorage.refresh();
    expect(historyStorage.overNotice()).toBe(false);
  });

  it("ignores a size that is not a positive number", () => {
    historyStorage.setNoticeBytes(-5);
    historyStorage.setNoticeBytes(Number.NaN);
    expect(historyStorage.noticeBytes()).toBe(DEFAULT_NOTICE_BYTES);
  });
});

describe("history text", () => {
  it("names the typed changes and who made them", () => {
    rememberSessionTitles([{ id: "s1", title: "Drum ideas" }]);
    expect(briefText({
      changes: { changes: [{ kind: "project_renamed", from: null, to: "Song" }], more: 0 },
      cause: "edit",
      author: { kind: "person" },
    })).toBe("Name the project “Song” (You)");
    expect(briefText({
      changes: { changes: [{ kind: "track_added", track: "groove" }], more: 0 },
      cause: "edit",
      author: { kind: "agent", session: "s1" },
    })).toBe("Add track “groove” (Agent - Drum ideas)");
    expect(briefText({
      changes: { changes: [{ kind: "tempo", from: 120, to: 132 }], more: 0 },
      cause: "edit",
      author: { kind: "agent", session: "gone" },
    })).toBe("Change tempo from 120 to 132 BPM (Agent)");
    expect(briefText({
      changes: { changes: [], more: 0 },
      cause: "edit",
      author: { kind: "outside" },
    })).toBe("Edit (Outside OpenCompose)");
  });

  it("sizes bytes in the largest unit that keeps a whole number", () => {
    expect(sizeText(512)).toBe("512 B");
    expect(sizeText(2048)).toBe("2.0 KB");
    expect(sizeText(5 * 1024 ** 2)).toBe("5.0 MB");
    expect(sizeText(1.5 * 1024 ** 3)).toBe("1.5 GB");
  });
});
