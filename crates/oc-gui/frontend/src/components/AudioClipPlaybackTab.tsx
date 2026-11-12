import { createEffect, createSignal, Index, Show } from "solid-js";
import { audioClipTransients, previewAudioClipSource } from "../stores/api";
import { i18n } from "../stores/i18n";
import { store } from "../stores/project";
import { InlineAudioPreview } from "./InlineAudioPreview";
import type { AudioClip, AudioWarpMarker } from "../types/project";

type StretchMode = NonNullable<AudioClip["stretch_mode"]>;

export function AudioClipPlaybackTab(props: { clip: AudioClip; index: number }) {
  const [previewOpen, setPreviewOpen] = createSignal(false);
  const [previewStart, setPreviewStart] = createSignal("0");
  const [previewLength, setPreviewLength] = createSignal("5");
  const validPreview = () => previewStart().trim() !== "" && Number(previewStart()) >= 0
    && Number.isFinite(Number(previewStart())) && Number(previewLength()) > 0 && Number(previewLength()) <= 8;
  const [speed, setSpeed] = createSignal("1");
  const [pitch, setPitch] = createSignal("0");
  const [formant, setFormant] = createSignal("0");
  const [preserve, setPreserve] = createSignal(false);
  const [markers, setMarkers] = createSignal<AudioWarpMarker[]>([]);
  const patchMarker = (index: number, key: keyof AudioWarpMarker, value: number) =>
    setMarkers((current) => current.map((marker, i) => i === index ? { ...marker, [key]: value } : marker));
  const addMarker = () => {
    const last = markers().at(-1);
    setMarkers([...markers(), {
      source_seconds: (last?.source_seconds ?? 0) + 0.25,
      target_seconds: (last?.target_seconds ?? 0) + 0.25,
    }]);
  };
  const [mode, setMode] = createSignal<StretchMode>("musical");
  const varispeed = () => mode() === "varispeed";
  const independentControls = () => mode() !== "off" && !varispeed();
  const [follow, setFollow] = createSignal(false);
  const [bypassed, setBypassed] = createSignal(false);
  const [snapping, setSnapping] = createSignal(false);
  const [snapMessage, setSnapMessage] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    const clip = props.clip;
    setSpeed(String(Number((1 / (clip.stretch ?? 1)).toPrecision(10))));
    setPitch(String(clip.pitch ?? 0));
    setFormant(String(clip.formant ?? 0));
    setPreserve(clip.preserve_formants ?? false);
    setMarkers(clip.warp_markers ?? []);
    setMode(clip.stretch_mode ?? "musical");
    setFollow(clip.tempo_follow ?? false);
    setBypassed(clip.bypass_processing ?? false);
    setError(null);
    setSnapMessage(null);
  });

  const snapMarker = async (index: number) => {
    const draft = markers();
    const source = draft[index].source_seconds;
    if (snapping() || !Number.isFinite(source) || source <= 0) return;
    const clip = props.clip;
    const clipIndex = props.index;
    const start = Math.max(0, source - 0.075);
    const isCurrent = () => props.clip === clip && props.index === clipIndex && markers() === draft;
    setSnapping(true);
    setSnapMessage(null);
    try {
      const result = await audioClipTransients(clipIndex, start, source + 0.075 - start, 8);
      if (!isCurrent()) return;
      const lower = draft[index - 1]?.source_seconds ?? 0;
      const upper = draft[index + 1]?.source_seconds ?? Infinity;
      const nearest = result.transients
        .map((event) => Number(event.source_seconds.toFixed(6)))
        .filter((seconds) => seconds > lower && seconds < upper)
        .sort((left, right) => Math.abs(left - source) - Math.abs(right - source))[0];
      if (nearest === undefined) {
        setSnapMessage(i18n.t("clipPlayback.snapMissing"));
      } else {
        patchMarker(index, "source_seconds", nearest);
      }
    } catch {
      if (isCurrent()) setSnapMessage(i18n.t("clipPlayback.snapUnavailable"));
    } finally {
      setSnapping(false);
    }
  };

  const apply = async (event: SubmitEvent) => {
    event.preventDefault();
    if (busy() || snapping()) return;
    setBusy(true);
    setError(null);
    const result = await store.setAudioClipPlayback(props.index, {
      stretch: mode() !== "off" && !follow() ? 1 / Number(speed()) : undefined,
      pitch: independentControls() ? Number(pitch()) : 0,
      formant: independentControls() ? Number(formant()) : 0,
      preserve_formants: independentControls() && preserve(),
      warp_markers: independentControls() ? markers() : [],
      stretch_mode: mode(),
      bypass_processing: bypassed(),
      tempo_follow: mode() !== "off" && follow(),
    });
    setError(result);
    setBusy(false);
  };

  return (
    <form class="gain-env-playback" onSubmit={(event) => void apply(event)}>
      <details class="clip-source-preview"
        onToggle={(event) => setPreviewOpen(event.currentTarget.open)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && event.target instanceof HTMLInputElement) event.preventDefault();
        }}>
        <summary>{i18n.t("clipPlayback.sourcePreview")}</summary>
        <label>
          {i18n.t("clipPlayback.previewStart")}
          <input type="number" step="any" value={previewStart()}
            onInput={(event) => setPreviewStart(event.currentTarget.value)} />
        </label>
        <label>
          {i18n.t("clipPlayback.previewLength")}
          <input type="number" step="any" value={previewLength()}
            onInput={(event) => setPreviewLength(event.currentTarget.value)} />
        </label>
        <InlineAudioPreview
          label={i18n.t("ai.media.loadAudition")}
          disabled={!validPreview()}
          resetKey={JSON.stringify([store.filePath(), props.index, props.clip.file, previewOpen(), previewStart(), previewLength()])}
          load={async () => (await previewAudioClipSource(props.index, Number(previewStart()), Number(previewLength()))).audio}
        />
      </details>
      <p class="gain-env-hint">{i18n.t("clipPlayback.hint")}</p>
      <label>
        {i18n.t("clipPlayback.mode")}
        <select value={mode()} onChange={(event) => setMode(event.currentTarget.value as StretchMode)}>
          <option value="musical">{i18n.t("clipPlayback.musical")}</option>
          <option value="percussive">{i18n.t("clipPlayback.percussive")}</option>
          <option value="varispeed">{i18n.t("clipPlayback.varispeed")}</option>
          <Show when={mode() === "off"}>
            <option value="off">{i18n.t("clipPlayback.off")}</option>
          </Show>
        </select>
      </label>
      <Show when={mode() !== "off"}>
        <label class="gain-env-playback-check">
          <input type="checkbox" checked={bypassed()} onChange={(event) => setBypassed(event.currentTarget.checked)} />
          {i18n.t("clipPlayback.bypass")}
        </label>
      </Show>
      <label class="gain-env-playback-check">
        <input type="checkbox" checked={follow() && mode() !== "off"} disabled={mode() === "off" || (!varispeed() && markers().length > 0)}
          onChange={(event) => setFollow(event.currentTarget.checked)} />
        {i18n.t("clipPlayback.fit")}
      </label>
      <label>
        {i18n.t("clipPlayback.speed")}
        <input type="number" min="0.25" max="4" step="any" required
          value={speed()} disabled={follow() || mode() === "off"}
          onInput={(event) => setSpeed(event.currentTarget.value)} />
      </label>
      <Show when={varispeed()}>
        <p class="gain-env-hint">{i18n.t("clipPlayback.varispeedHint")}</p>
      </Show>
      <Show when={!varispeed()}>
        <label>
          {i18n.t("clipPlayback.pitch")}
          <input type="number" min="-24" max="24" step="any" required
            value={mode() === "off" ? "0" : pitch()} disabled={mode() === "off"}
            onInput={(event) => setPitch(event.currentTarget.value)} />
        </label>
        <label class="gain-env-playback-check">
          <input type="checkbox" checked={preserve() && mode() !== "off"} disabled={mode() === "off"}
            onChange={(event) => setPreserve(event.currentTarget.checked)} />
          {i18n.t("clipPlayback.preserve")}
        </label>
        <label>
          {i18n.t("clipPlayback.formant")}
          <input type="number" min="-12" max="12" step="any" required
            value={mode() === "off" ? "0" : formant()} disabled={mode() === "off"}
            onInput={(event) => setFormant(event.currentTarget.value)} />
        </label>
        <p class="gain-env-hint">{i18n.t("clipPlayback.formants")}</p>
        <fieldset disabled={mode() === "off" || follow()} class="gain-env-warp">
          <legend>{i18n.t("clipPlayback.warp")}</legend>
          <p class="gain-env-hint">{i18n.t(follow() ? "clipPlayback.disableFitForWarp" : "clipPlayback.warpHint")}</p>
          <Index each={markers()}>
            {(marker, index) => (
              <div class="gain-env-warp-row">
                <label>{i18n.t("clipPlayback.source")}
                  <input type="number" min="0.000001" step="any" required value={marker().source_seconds}
                    onChange={(event) => patchMarker(index, "source_seconds", event.currentTarget.valueAsNumber)} />
                </label>
                <label>{i18n.t("clipPlayback.target")}
                  <input type="number" min="0.000001" step="any" required value={marker().target_seconds}
                    onChange={(event) => patchMarker(index, "target_seconds", event.currentTarget.valueAsNumber)} />
                </label>
                <div class="gain-env-warp-actions">
                  <button type="button" class="gain-env-add"
                    disabled={snapping() || busy() || !(marker().source_seconds > 0)}
                    aria-label={i18n.t("clipPlayback.snapSource")}
                    onClick={() => void snapMarker(index)}>{i18n.t("clipPlayback.snap")}</button>
                  <button type="button" class="gain-env-add" aria-label={i18n.t("clipPlayback.removeMarker")}
                    onClick={() => setMarkers((current) => current.filter((_, i) => i !== index))}>×</button>
                </div>
              </div>
            )}
          </Index>
          <Show when={snapMessage()}>{(message) => <p class="gain-env-hint" role="status">{message()}</p>}</Show>
          <button type="button" class="gain-env-add" disabled={markers().length >= 128} onClick={addMarker}>
            {i18n.t("clipPlayback.addMarker")}
          </button>
        </fieldset>
      </Show>
      <Show when={error()}>{(message) => <p role="alert">{message()}</p>}</Show>
      <div><button class="gain-env-add" type="submit" disabled={busy() || snapping()}>{i18n.t("clipPlayback.apply")}</button></div>
    </form>
  );
}
