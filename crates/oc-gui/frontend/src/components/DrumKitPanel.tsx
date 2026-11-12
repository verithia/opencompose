/**
 * Drum-kit pad grid. Pads audition through the backend kit instrument, sample
 * drops and inspector edits go through `setPresetParams` so undo stays intact.
 */
import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { midiStore } from "../stores/midiLive";
import type { PresetDef } from "../types/project";
import {
  DEFAULT_PAD_NOTES,
  isDrumKitPreset,
  kitFromPreset,
  midiNoteName,
  padParamUpdates,
  roundRobinCount,
  sampleFileName,
  sanitizePadName,
  withPadSample,
  type KitPad,
} from "../utils/drumkit";
import "./DrumKitPanel.css";

interface PadSlot {
  key: string;
  index: number;
  pad: KitPad | null;
  defaultNote: number;
}

export function DrumKitPanel() {
  const [selectedName, setSelectedName] = createSignal<string | null>(null);
  const [flashing, setFlashing] = createSignal<number[]>([]);
  const [dragSlot, setDragSlot] = createSignal<number | null>(null);
  const [status, setStatus] = createSignal("Click a pad to audition; drop a WAV or FLAC to load it.");
  const flashTimers = new Map<number, number>();
  let lastMidiMillis = 0;

  const preset = createMemo<PresetDef | null>(() => {
    const track = store.selectedTrack();
    if (track?.preset) {
      const candidate = store.project.presets.find((entry) => entry.name === track.preset);
      if (candidate && isDrumKitPreset(candidate)) return candidate;
    }
    const selected = store.selectedPresetName();
    if (selected) {
      const candidate = store.project.presets.find((entry) => entry.name === selected);
      if (candidate && isDrumKitPreset(candidate)) return candidate;
    }
    return store.project.presets.find((entry) => isDrumKitPreset(entry)) ?? null;
  });

  const kit = createMemo(() => {
    const current = preset();
    return current ? kitFromPreset(current) : null;
  });

  const pads = createMemo(() => {
    const current = kit();
    if (!current) return [];
    return [...current.pads].sort((left, right) => left.note - right.note);
  });

  const slots = createMemo<PadSlot[]>(() => {
    const ordered = pads();
    const count = Math.max(16, ordered.length);
    return Array.from({ length: count }, (_, index) => {
      const pad = ordered[index] ?? null;
      return {
        key: pad?.name ?? `empty-${index}`,
        index,
        pad,
        defaultNote: DEFAULT_PAD_NOTES[index] ?? 36 + index,
      };
    });
  });

  const selectedPad = createMemo(() => pads().find((pad) => pad.name === selectedName()) ?? null);

  function flash(note: number) {
    setFlashing((previous) => (previous.includes(note) ? previous : [...previous, note]));
    const timer = flashTimers.get(note);
    if (timer !== undefined) clearTimeout(timer);
    flashTimers.set(
      note,
      window.setTimeout(() => {
        setFlashing((previous) => previous.filter((entry) => entry !== note));
        flashTimers.delete(note);
      }, 140),
    );
  }

  createEffect(() => {
    const last = midiStore.activity().last;
    if (!last || last.kind !== "note_on" || last.note == null) return;
    if (last.millis === lastMidiMillis) return;
    lastMidiMillis = last.millis;
    flash(last.note);
  });

  onCleanup(() => {
    for (const timer of flashTimers.values()) clearTimeout(timer);
    flashTimers.clear();
  });

  async function audition(pad: KitPad) {
    flash(pad.note);
    setSelectedName(pad.name);
    const current = preset();
    if (!current) return;
    await store.previewDrumPad(current.name, pad.note, 110);
  }

  async function loadSample(slotIndex: number, path: string) {
    const currentKit = kit();
    const currentPreset = preset();
    if (!currentKit || !currentPreset) {
      setStatus("No drum kit in this project. Add a @kit block first.");
      return;
    }
    const existing = pads()[slotIndex];
    const name = existing?.name ?? sanitizePadName(`pad_${slotIndex + 1}`);
    const updates = withPadSample(currentKit, name, path);
    setSelectedName(name);
    await store.setPresetParams(currentPreset.name, updates);
    setStatus(`Loaded ${sampleFileName(path)} onto ${name}.`);
  }

  function dropOnSlot(event: DragEvent, slotIndex: number) {
    event.preventDefault();
    setDragSlot(null);
    const file = event.dataTransfer?.files?.[0];
    const path = file ? ((file as File & { path?: string }).path ?? file.name) : null;
    if (path) void loadSample(slotIndex, path);
  }

  // Tauri intercepts native file drops; map the drop position back to a pad.
  onMount(() => {
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        unlisten = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload as {
            type?: string;
            paths?: string[];
            position?: { x: number; y: number };
          };
          if (payload.type !== "drop" || !payload.paths?.length) return;
          const scale = window.devicePixelRatio || 1;
          const x = (payload.position?.x ?? 0) / scale;
          const y = (payload.position?.y ?? 0) / scale;
          const target = document.elementFromPoint(x, y)?.closest("[data-pad-slot]");
          const index = target instanceof HTMLElement ? Number(target.dataset.padSlot) : NaN;
          if (Number.isFinite(index)) void loadSample(index, payload.paths[0]);
        });
      } catch {
        // Browser dev mode: the DOM drop handler above handles it.
      }
    })();
    onCleanup(() => unlisten?.());
  });

  function editPad(update: Partial<KitPad>) {
    const pad = selectedPad();
    const current = preset();
    if (!pad || !current) return;
    const next: KitPad = {
      ...pad,
      ...update,
      layers: pad.layers.map((layer) => ({ ...layer })),
    };
    void store.setPresetParams(current.name, padParamUpdates(next));
  }

  function numberField(
    label: string,
    value: number,
    step: number,
    update: (value: number) => Partial<KitPad>,
  ) {
    return (
      <label class="drum-field">
        <span>{label}</span>
        <input
          type="number"
          step={step}
          value={value}
          onChange={(event) => {
            const parsed = Number.parseFloat(event.currentTarget.value);
            if (Number.isFinite(parsed)) editPad(update(parsed));
          }}
        />
      </label>
    );
  }

  return (
    <div class="drumkit-panel">
      <div class="drumkit-main">
        <div class="drumkit-header">
          <strong>{kit()?.name ?? i18n.t("drumkit.noKit")}</strong>
          <span class="drumkit-muted">
            {kit()
              ? i18n.t("drumkit.padVoices", { pads: kit()!.pads.length, voices: kit()!.maxVoices })
              : i18n.t("drumkit.addKit")}
          </span>
        </div>
        <div class="drumkit-grid">
          <For each={slots()}>
            {(slot) => (
              <button
                type="button"
                class="drum-pad"
                classList={{
                  empty: !slot.pad,
                  selected: slot.pad !== null && selectedPad()?.name === slot.pad.name,
                  flashing: slot.pad !== null && flashing().includes(slot.pad.note),
                  "drag-target": dragSlot() === slot.index,
                }}
                data-pad-slot={slot.index}
                onClick={() => (slot.pad ? void audition(slot.pad) : setSelectedName(null))}
                onDragOver={(event) => {
                  event.preventDefault();
                  setDragSlot(slot.index);
                }}
                onDragLeave={() => setDragSlot(null)}
                onDrop={(event) => dropOnSlot(event, slot.index)}
              >
                <span class="drum-pad-name">{slot.pad?.name ?? i18n.t("drumkit.padName", { number: slot.index + 1 })}</span>
                <span class="drum-pad-sample">
                  {slot.pad
                    ? slot.pad.layers[0]?.sample
                      ? sampleFileName(slot.pad.layers[0].sample)
                      : i18n.t("drumkit.noSample")
                    : i18n.t("drumkit.dropSample")}
                </span>
                <span class="drum-pad-meta">
                  {midiNoteName(slot.pad?.note ?? slot.defaultNote)}
                  {slot.pad ? ` · ${slot.pad.layers.length}L · ${roundRobinCount(slot.pad)}RR` : ""}
                  {slot.pad && slot.pad.choke > 0 ? ` · ${i18n.t("drumkit.chokeMeta", { value: slot.pad.choke })}` : ""}
                </span>
              </button>
            )}
          </For>
        </div>
        <div class="drumkit-status">{status()}</div>
      </div>

      <Show when={selectedPad()}>
        {(pad) => (
          <aside class="drumkit-inspector">
            <div class="drumkit-inspector-title">
              {pad().name}
              <span class="drumkit-muted"> · {midiNoteName(pad().note)}</span>
            </div>
            {numberField(i18n.t("drumkit.gain"), pad().gainDb, 0.5, (gainDb) => ({ gainDb }))}
            {numberField(i18n.t("routing.pan"), pad().pan, 0.05, (pan) => ({ pan }))}
            {numberField(i18n.t("drumkit.tune"), pad().tune, 1, (tune) => ({ tune }))}
            {numberField(i18n.t("drumkit.cents"), pad().cents, 1, (cents) => ({ cents }))}
            {numberField(i18n.t("drumkit.decay"), pad().decay, 0.01, (decay) => ({ decay }))}
            {numberField(i18n.t("drumkit.release"), pad().release, 0.005, (release) => ({ release }))}
            {numberField(i18n.t("drumkit.choke"), pad().choke, 1, (choke) => ({ choke }))}
            <label class="drum-field">
              <span>{i18n.t("synth.param.mode")}</span>
              <select
                value={pad().oneShot ? "one_shot" : "gate"}
                onChange={(event) => editPad({ oneShot: event.currentTarget.value === "one_shot" })}
              >
                <option value="one_shot">{i18n.t("drumkit.oneShot")}</option>
                <option value="gate">{i18n.t("drumkit.gate")}</option>
              </select>
            </label>
            <label class="drum-field drum-field-check">
              <input
                type="checkbox"
                checked={pad().reverse}
                onChange={(event) => editPad({ reverse: event.currentTarget.checked })}
              />
              <span>{i18n.t("drumkit.reverse")}</span>
            </label>

            <div class="drumkit-layers">
              <div class="drumkit-layers-title">{i18n.t("drumkit.layers")}</div>
              <Show when={pad().layers.length > 0} fallback={<div class="drumkit-muted">{i18n.t("drumkit.noSamples")}</div>}>
                <For each={pad().layers}>
                  {(layer, index) => (
                    <div class="drumkit-layer-row">
                      <span>
                        {index()}: {layer.low}–{layer.high}
                      </span>
                      <span class="drumkit-muted">{sampleFileName(layer.sample) || i18n.t("drumkit.missing")}</span>
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </aside>
        )}
      </Show>
    </div>
  );
}
