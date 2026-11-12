import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { i18n } from "../stores/i18n";
import type { PreviewAudio } from "../stores/api";
import { PreviewAudioPlayer } from "./PreviewAudioPlayer";

export function InlineAudioPreview(props: {
  load: () => Promise<PreviewAudio | string>;
  resetKey: string;
  label: string;
  disabled?: boolean;
}) {
  const [source, setSource] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  let request = 0;
  createEffect(() => {
    void props.resetKey;
    request++;
    setSource(null);
    setBusy(false);
    setFailed(false);
  });
  onCleanup(() => { request++; });

  const prepare = async () => {
    const id = ++request;
    setBusy(true);
    setFailed(false);
    try {
      const audio = await props.load();
      if (id === request) setSource(typeof audio === "string" ? audio : `data:${audio.mime};base64,${audio.base64}`);
    } catch {
      if (id === request) setFailed(true);
    } finally {
      if (id === request) setBusy(false);
    }
  };

  return (
    <div class="inline-audio-preview">
      <Show when={source()} fallback={
        <button class="toolbar-btn text-btn" type="button" disabled={busy() || props.disabled}
          onClick={() => void prepare()}>
          {busy() ? i18n.t("ai.tool.audioPreparing") : props.label}
        </button>
      }>
        {(url) => <PreviewAudioPlayer src={url()} label={props.label} volumeControl
          onError={() => { setSource(null); setFailed(true); }} />}
      </Show>
      <Show when={failed()}>
        <span role="status">{i18n.t("ai.tool.audioFailed")}</span>
      </Show>
    </div>
  );
}
