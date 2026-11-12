import { createSignal, onCleanup, Show } from "solid-js";
import { nativePlaybackBlocker } from "../stores/api";
import { i18n } from "../stores/i18n";
import { playWebAudio, releaseWebAudio, webAudioAuthorized, type NativeAudioBlocker } from "../stores/webAudioFocus";

const NOTICE_LABEL = {
  busy: "ai.tool.audioNativeBusy",
  monitoring: "ai.tool.audioMonitoringBusy",
  synth: "ai.tool.audioSynthBusy",
  failed: "ai.tool.audioFailed",
} as const;

function formatClock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

export function PreviewAudioPlayer(props: {
  src: string;
  label?: string;
  duration?: number;
  volumeControl?: boolean;
  onError?: () => void;
}) {
  const [playing, setPlaying] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const [notice, setNotice] = createSignal<NativeAudioBlocker | "failed" | null>(null);
  const [position, setPosition] = createSignal(0);
  const [duration, setDuration] = createSignal(props.duration ?? 0);
  const [seeking, setSeeking] = createSignal(false);
  let audio!: HTMLAudioElement;
  let request = 0;
  let pendingSeek: { source: string; seconds: number } | undefined;

  const pause = () => {
    ++request;
    releaseWebAudio(audio);
    if (!audio.paused) audio.pause();
    setPending(false);
    setPlaying(false);
  };
  onCleanup(pause);

  const toggle = async () => {
    if (pending() || !audio.paused) {
      pause();
      return;
    }
    const id = ++request;
    setNotice(null);
    setPending(true);
    const result = await playWebAudio(audio, nativePlaybackBlocker);
    if (id !== request) return;
    setPending(false);
    setPlaying(result === "playing");
    if (result !== "playing" && result !== "stale") setNotice(result);
  };

  return (
    <>
      <div class="ai-tool-audio-controls" role="group" aria-label={props.label}
        onKeyDown={(event) => { if (event.key === " ") event.stopPropagation(); }}>
        <button type="button" class="ai-tool-audio-toggle" aria-busy={pending()}
          aria-label={i18n.t(pending() ? "dialog.cancel" : playing() ? "ai.tool.pause" : "ai.tool.play")}
          onClick={() => void toggle()}>{pending() ? "×" : playing() ? "❚❚" : "▶"}</button>
        <input class="ai-tool-audio-seek" type="range" min="0" max={duration() || 1}
          step="0.01" value={position()} disabled={duration() <= 0} aria-label={i18n.t("ai.tool.seek")}
          onInput={(event) => {
            const seconds = Number(event.currentTarget.value);
            setPosition(seconds);
            if (audio.readyState > 0) {
              pendingSeek = undefined;
              audio.currentTime = seconds;
            } else {
              pendingSeek = { source: props.src, seconds };
            }
          }}
          onPointerDown={() => setSeeking(true)} onPointerUp={() => setSeeking(false)}
          onPointerCancel={() => setSeeking(false)} onBlur={() => setSeeking(false)} />
        <span class="ai-tool-audio-clock">{formatClock(position())} / {formatClock(duration())}</span>
        <Show when={props.volumeControl}>
          <input class="ai-tool-audio-volume" type="range" min="0" max="1" step="0.05" value="1"
            aria-label={i18n.t("ai.tool.audioVolume")}
            onInput={(event) => { audio.volume = Number(event.currentTarget.value); }} />
        </Show>
      </div>
      <audio ref={audio} class="ai-tool-audio-element" hidden preload="metadata" src={props.src}
        onLoadedMetadata={(event) => {
          const seconds = event.currentTarget.duration;
          if (Number.isFinite(seconds) && seconds > 0) setDuration(seconds);
          const seek = pendingSeek;
          pendingSeek = undefined;
          if (seek?.source === props.src) {
            const target = Math.min(seek.seconds, duration());
            event.currentTarget.currentTime = target;
            setPosition(target);
          }
        }}
        onTimeUpdate={(event) => {
          if (!seeking() && pendingSeek?.source !== props.src) setPosition(event.currentTarget.currentTime);
        }}
        onPlay={(event) => {
          // A platform media action can reach the hidden element directly.
          if (!webAudioAuthorized(event.currentTarget)) event.currentTarget.pause();
        }}
        onPause={(event) => {
          if (event.currentTarget.paused && !pending()) pause();
        }}
        onEnded={() => {
          pause();
          setPosition(0);
          audio.currentTime = 0;
        }}
        onError={() => {
          pause();
          if (props.onError) props.onError();
          else setNotice("failed");
        }} />
      <Show when={notice()}>
        {(reason) => <div class="ai-tool-audio-note" role="status">{i18n.t(NOTICE_LABEL[reason()])}</div>}
      </Show>
    </>
  );
}
