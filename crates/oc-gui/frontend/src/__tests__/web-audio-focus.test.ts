import { afterEach, describe, expect, it, vi } from "vitest";
import { transportPlay } from "../stores/api";
import { playWebAudio, pauseWebAudio, releaseWebAudio, startNativeAudio, webAudioAuthorized, type NativeAudioBlocker } from "../stores/webAudioFocus";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

function webPlayer() {
  const player = {
    paused: true,
    play: vi.fn(async () => { player.paused = false; }),
    pause: vi.fn(() => { player.paused = true; }),
  };
  return { player, element: player as unknown as HTMLAudioElement };
}

afterEach(() => { pauseWebAudio(); });

describe("web and native playback ownership", () => {
  it("acknowledges an agent handoff only after pausing the matching player", async () => {
    const { player, element } = webPlayer();
    expect(await playWebAudio(element, async () => null)).toBe("playing");
    const listener = vi.mocked(listen).mock.calls.find(([event]) => event === "native-playback-starting");
    expect(listener?.[2]).toEqual({ target: "main" });
    const claim = vi.mocked(invoke).mock.calls.find(([command]) => command === "claim_web_audio");
    const { token } = claim?.[1] as { token: string };
    listener?.[1]({ event: "native-playback-starting", id: 1, payload: token });
    expect(player.paused).toBe(true);
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("release_web_audio", { token }));
  });

  it("waits for the native check before calling play and refuses native overlap", async () => {
    const { player, element } = webPlayer();
    let resolve!: (blocker: NativeAudioBlocker | null) => void;
    const ready = playWebAudio(element, () => new Promise<NativeAudioBlocker | null>(done => { resolve = done; }));
    expect(player.play).not.toHaveBeenCalled();
    expect(webAudioAuthorized(element)).toBe(false);
    await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
    resolve("busy");
    expect(await ready).toBe("busy");
    expect(player.play).not.toHaveBeenCalled();
  });

  it("pauses the other web player before starting the new one", async () => {
    const first = webPlayer();
    const second = webPlayer();
    expect(await playWebAudio(first.element, async () => null)).toBe("playing");
    expect(webAudioAuthorized(first.element)).toBe(true);
    second.player.play.mockImplementationOnce(async () => {
      expect(first.player.paused).toBe(true);
    });
    expect(await playWebAudio(second.element, async () => null)).toBe("playing");
    expect(first.player.pause).toHaveBeenCalledOnce();
    expect(webAudioAuthorized(first.element)).toBe(false);
  });

  it("keeps playback stopped if the native check or browser play fails", async () => {
    const { player, element } = webPlayer();
    expect(await playWebAudio(element, async () => { throw new Error("IPC unavailable"); })).toBe("failed");
    expect(player.play).not.toHaveBeenCalled();
    player.play.mockRejectedValueOnce(new Error("Cannot play"));
    expect(await playWebAudio(element, async () => null)).toBe("failed");
    expect(pauseWebAudio()).toBe(false);
  });

  it("fences a stale native check when the transport starts", async () => {
    const { player, element } = webPlayer();
    let resolve!: (blocker: NativeAudioBlocker | null) => void;
    const ready = playWebAudio(element, () => new Promise<NativeAudioBlocker | null>(done => { resolve = done; }));
    await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
    vi.mocked(invoke).mockResolvedValueOnce({ playing: true });
    await transportPlay();
    expect(invoke).toHaveBeenCalledWith("transport_play");
    resolve(null);
    expect(await ready).toBe("stale");
    expect(player.play).not.toHaveBeenCalled();
  });

  it("pauses active web audio before invoking the native transport", async () => {
    const { player, element } = webPlayer();
    await playWebAudio(element, async () => null);
    vi.mocked(invoke).mockImplementationOnce(async () => {
      expect(player.paused).toBe(true);
      return { playing: true };
    });
    await transportPlay();
    expect(player.pause).toHaveBeenCalledOnce();
  });

  it("refuses a browser play during native startup and can cancel a pending web play", async () => {
    const { player, element } = webPlayer();
    let finish!: () => void;
    const pending = startNativeAudio(() => new Promise<void>(done => { finish = done; }));
    const check = vi.fn(async () => null);
    expect(await playWebAudio(element, check)).toBe("busy");
    expect(check).not.toHaveBeenCalled();
    finish();
    await pending;
    const ready = playWebAudio(element, check);
    releaseWebAudio(element);
    expect(await ready).toBe("stale");
    expect(player.play).not.toHaveBeenCalled();
  });
});
