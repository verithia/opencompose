import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { log } from "./log";

let owner: { player: HTMLAudioElement; authorized: boolean; token: string } | undefined;
let nativeStarts = 0;
let handoff: Promise<unknown> | undefined;
let leaseTail: Promise<unknown> = Promise.resolve();
export type NativeAudioBlocker = "busy" | "monitoring" | "synth";

const claimWebAudioLease = (token: string) =>
  invoke<boolean>("claim_web_audio", { token });
const releaseWebAudioLease = (token: string) =>
  invoke<void>("release_web_audio", { token });

function orderedLease<T>(operation: () => Promise<T>): Promise<T> {
  const result = leaseTail.then(operation);
  leaseTail = result.catch(() => undefined);
  return result;
}

function releaseLease(token: string) {
  void orderedLease(() => releaseWebAudioLease(token))
    .catch(error => log.warn("audio", String(error)));
}

export function prepareWebAudioHandoff(): Promise<unknown> {
  handoff ??= listen<string>("native-playback-starting", ({ payload }) => {
    if (owner?.token === payload) pauseWebAudio();
    else releaseLease(payload);
  }, { target: getCurrentWindow().label }).catch(error => {
    handoff = undefined;
    throw error;
  });
  return handoff;
}

export function releaseWebAudio(player: HTMLAudioElement) {
  if (owner?.player !== player) return;
  const { token } = owner;
  owner = undefined;
  releaseLease(token);
}

export function webAudioAuthorized(player: HTMLAudioElement): boolean {
  return owner?.player === player && owner.authorized;
}

export function pauseWebAudio() {
  if (!owner) return false;
  const { player } = owner;
  if (!player.paused) player.pause();
  releaseWebAudio(player);
  return true;
}

export async function startNativeAudio<T>(start: () => Promise<T>): Promise<T> {
  nativeStarts++;
  try {
    pauseWebAudio();
    return await start();
  } finally {
    nativeStarts--;
  }
}

export async function playWebAudio(
  player: HTMLAudioElement,
  nativeBlocker: () => Promise<NativeAudioBlocker | null>,
): Promise<"playing" | NativeAudioBlocker | "failed" | "stale"> {
  if (nativeStarts) return "busy";
  pauseWebAudio();
  const claim = { player, authorized: false, token: crypto.randomUUID() };
  owner = claim;
  try {
    const acquired = await orderedLease(async () => {
      await prepareWebAudioHandoff();
      return owner === claim && await claimWebAudioLease(claim.token);
    });
    if (owner !== claim) return "stale";
    const blocker = acquired ? await nativeBlocker() : "busy";
    if (owner !== claim) return "stale";
    if (blocker) {
      releaseWebAudio(player);
      return blocker;
    }
    claim.authorized = true;
    await player.play();
    return owner === claim ? "playing" : "stale";
  } catch {
    if (owner !== claim) return "stale";
    pauseWebAudio();
    return "failed";
  }
}
