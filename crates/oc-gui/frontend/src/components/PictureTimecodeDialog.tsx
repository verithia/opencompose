import { createEffect, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { pictureTimecode, type PicturePoint, type PictureRate } from "../stores/api";
import { i18n } from "../stores/i18n";
import { store } from "../stores/project";
import { previewStore } from "../stores/preview";
import { projectClock } from "../utils/projectClock";

const rates: [PictureRate, string][] = [
  ["24000/1001", "23.976"], ["24", "24"], ["25", "25"], ["30000/1001", "29.97"], ["30", "30"],
  ["48000/1001", "47.95"], ["48", "48"], ["50", "50"], ["60000/1001", "59.94"], ["60", "60"],
];

export function PictureTimecodeDialog(props: {
  rate: PictureRate;
  origin: string;
  onClock: (rate: PictureRate, origin: string) => void;
  onClose: () => void;
}) {
  const [rate, setRate] = createSignal(props.rate);
  const [origin, setOrigin] = createSignal(props.origin);
  const [target, setTarget] = createSignal(props.origin);
  const [report, setReport] = createSignal<PicturePoint | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let input: HTMLInputElement | undefined;
  let request = 0;
  let generation = store.documentIdentity().generation;
  onMount(() => input?.focus());
  onCleanup(() => { request += 1; });
  // A report and its target belong to one document revision. Replacement
  // invalidates both, even when no request is in flight.
  createEffect(() => {
    const next = store.documentIdentity().generation;
    if (next === generation) return;
    generation = next;
    request += 1;
    setBusy(false);
    setReport(null);
    setError("");
  });

  const clear = () => {
    request += 1;
    setBusy(false);
    setReport(null);
    setError("");
  };
  const resolve = async (fromPlayhead: boolean) => {
    if (busy()) return;
    const id = ++request;
    const generation = store.documentIdentity().generation;
    setBusy(true);
    setReport(null);
    setError("");
    try {
      const point = fromPlayhead
        ? { seconds: projectClock(store.project.properties).secondsAt(
          store.previewCursorBeats() ?? store.transport().position_beats,
        ) }
        : { timecode: target().trim() };
      const response = await pictureTimecode(rate(), origin().trim(), [point]);
      if (id !== request || store.documentIdentity().generation !== generation) return;
      setReport(response.points[0]);
      if (fromPlayhead) setTarget(response.points[0].timecode);
      props.onClock(rate(), origin().trim());
    } catch (failure) {
      if (id === request && store.documentIdentity().generation === generation) {
        setError(String(failure));
      }
    } finally {
      if (id === request && store.documentIdentity().generation === generation) {
        setBusy(false);
      }
    }
  };

  const saveClock = async (remove: boolean) => {
    if (busy() || previewStore.preview() !== null) return;
    const id = ++request;
    setBusy(true);
    setError("");
    const failure = await store.setPictureClock(
      remove ? null : { rate: rate(), origin: origin().trim() },
      store.projectRevision,
    );
    if (id !== request) return;
    if (failure !== null) setError(failure);
    setBusy(false);
  };

  return (
    <div class="text-prompt-overlay" onPointerDown={(event) => {
      if (event.target === event.currentTarget) props.onClose();
    }}>
      <form class="text-prompt picture-timecode-dialog" role="dialog" aria-modal="true" aria-label={i18n.t("picture.title")}
        onSubmit={(event) => { event.preventDefault(); void resolve(false); }}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.key === "Escape") { event.preventDefault(); props.onClose(); }
        }}>
        <label class="text-prompt-label">
          <span>{i18n.t("picture.rate")}</span>
          <select class="text-prompt-input" disabled={busy()} value={rate()} onChange={(event) => {
            setRate(event.currentTarget.value as PictureRate); clear();
          }}>
            <For each={rates}>{([value, label]) => <option value={value}>{label}</option>}</For>
          </select>
        </label>
        <label class="text-prompt-label">
          <span>{i18n.t("picture.origin")}</span>
          <input class="text-prompt-input" disabled={busy()} value={origin()} onInput={(event) => {
            setOrigin(event.currentTarget.value); clear();
          }} />
        </label>
        <label class="text-prompt-label">
          <span>{i18n.t("picture.target")}</span>
          <input ref={input} class="text-prompt-input" disabled={busy()} value={target()} onInput={(event) => {
            setTarget(event.currentTarget.value); clear();
          }} />
        </label>
        <p>{i18n.t("picture.hint")}</p>
        <Show when={report()}>{(point) => (
          <p role="status">
            {point().timecode} · {i18n.t("picture.seconds", { seconds: point().frame_seconds.toFixed(3) })}
          </p>
        )}</Show>
        <Show when={error()}><p role="alert">{error()}</p></Show>
        <div class="text-prompt-actions">
          <button type="button" class="text-prompt-btn"
            disabled={busy() || !origin().trim() || previewStore.preview() !== null}
            onClick={() => void saveClock(false)}>{i18n.t("picture.saveClock")}</button>
          <Show when={store.project.properties.picture_clock}>
            <button type="button" class="text-prompt-btn" disabled={busy() || previewStore.preview() !== null}
              onClick={() => void saveClock(true)}>{i18n.t("picture.clearClock")}</button>
          </Show>
        </div>
        <div class="text-prompt-actions">
          <button type="button" class="text-prompt-btn" onClick={props.onClose}>{i18n.t("dialog.cancel")}</button>
          <button type="button" class="text-prompt-btn" disabled={busy()} onClick={() => void resolve(true)}>
            {i18n.t("picture.read")}
          </button>
          <button type="submit" class="text-prompt-btn" disabled={busy() || !target().trim()}>
            {i18n.t("picture.convert")}
          </button>
          <button type="button" class="text-prompt-btn primary" disabled={busy() || report()?.quarter == null || previewStore.preview() !== null}
            onClick={() => {
              const point = report();
              if (point?.quarter == null) return;
              const quarter = projectClock(store.project.properties).quarterAtSeconds(point.frame_seconds);
              void (async () => {
                // Keep the dialog open on a refused seek; the transport keeps
                // its own error state for the toolbar to render.
                if (await store.seek(quarter)) props.onClose();
              })();
            }}>{i18n.t("picture.seek")}</button>
        </div>
      </form>
    </div>
  );
}
