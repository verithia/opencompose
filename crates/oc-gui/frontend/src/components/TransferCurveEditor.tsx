import { createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type { Effect } from "../types/project";
import { store } from "../stores/project";
import { withParameterTarget } from "../stores/parameterAdmission";
import { i18n } from "../stores/i18n";
import { curveAsset, curvePath, decodeCurve, type TransferCurve } from "../devices/transferCurve";
import { createParameterGesture } from "../utils/parameterGesture";
import "../styles/transfer-curve.css";

export function TransferCurveEditor(props: { target: string; index: number; effect: Effect }) {
  const raw = () => {
    const value = props.effect.params.find(([name]) => name === "curve")?.[1];
    return value && "String" in value ? value.String : curveAsset().default_json;
  };
  const [draft, setDraft] = createSignal<TransferCurve | null>(null);
  const [point, setPoint] = createSignal(1);
  const [error, setError] = createSignal(false);
  let disposed = false;
  let operation = 0;
  const parsed = createMemo(() => decodeCurve(raw()));
  const curve = () => draft() ?? parsed();
  function capture() {
    const target = props.target, index = props.index, original = raw(), identity = ++operation;
    const owner = withParameterTarget(store.parameterRequestOwner(), () => !disposed && operation === identity && props.target === target && props.index === index
      && props.effect.effect_type === "TransferShaper" && raw() === original);
    return { target, index, owner, identity };
  }
  async function commit(text: string, captured = capture()) {
    try {
      decodeCurve(text);
      if (!captured.owner()) return;
      setError(false);
      await store.setEffectParams(captured.target, captured.index, [["curve", text]], captured.owner);
    } catch { if (captured.owner()) setError(true); }
    finally { if (operation === captured.identity) setDraft(null); }
  }
  const gesture = createParameterGesture({
    value: () => curve().samples[point()],
    draft: value => { const c = curve(); const samples = c.samples.slice(); samples[point()] = value; setDraft({ ...c, samples }); },
    clearDraft: () => setDraft(null),
    commit: () => {},
    captureCommit: () => {
      const captured = capture(), selected = point(), base = curve();
      return value => { const samples = base.samples.slice(); samples[selected] = value; void commit(JSON.stringify({ ...base, samples }), captured); };
    },
    surface: () => null, quarter: () => 0,
  });
  onCleanup(() => { disposed = true; gesture.dispose(); });
  createEffect(() => { raw(); props.target; props.index; gesture.interrupt(); setPoint(p => Math.min(p, decodeCurve(raw()).samples.length - 1)); });
  const lockedZero = () => curve().domain === "odd" && point() === 0
    || curve().domain === "bipolar" && point() === (curve().samples.length - 1) / 2;
  async function importFile(file: File | undefined) {
    if (!file) return;
    const captured = capture();
    if (file.size > curveAsset().max_bytes) { setError(true); return; }
    try { const text = await file.text(); if (captured.owner()) await commit(text, captured); }
    catch { if (captured.owner()) setError(true); }
  }
  function exportFile() {
    const url = URL.createObjectURL(new Blob([raw()], { type: "application/json" }));
    const link = document.createElement("a"); link.href = url; link.download = "transfer-curve.json"; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
  return <section class="transfer-curve" aria-label={i18n.t("curve.title")}>
    <div class="transfer-curve-toolbar">
      <span>{i18n.t("curve.title")}</span>
      <label class="transfer-curve-import">{i18n.t("curve.import")}<input type="file" accept=".json,application/json" aria-label={i18n.t("curve.import")}
        onChange={event => { void importFile(event.currentTarget.files?.[0]); event.currentTarget.value = ""; }} /></label>
      <button onClick={exportFile}>{i18n.t("curve.export")}</button>
      <button onClick={() => void commit(curveAsset().default_json)}>{i18n.t("curve.reset")}</button>
    </div>
    <svg viewBox="0 0 320 120" role="img" aria-label={i18n.t("curve.title")} onClick={event => {
      const box = event.currentTarget.getBoundingClientRect();
      setPoint(Math.max(0, Math.min(curve().samples.length - 1, Math.round((event.clientX - box.left) / box.width * (curve().samples.length - 1)))));
    }}><path class="transfer-curve-axis" d={curve().domain === "odd" ? "M0 60 H320 M0 0 V120" : "M0 60 H320 M160 0 V120"} /><path class="transfer-curve-line" d={curvePath(curve())} /></svg>
    <div class="transfer-curve-controls">
      <span>{i18n.t(curve().domain === "odd" ? "curve.odd" : "curve.bipolar")}</span>
      <label>{i18n.t("curve.samples")}<input type="number" min="2" max={curveAsset().max_samples} value={curve().samples.length} onChange={event => {
        const count = Number(event.currentTarget.value), c = curve();
        if (!Number.isInteger(count) || count < 2 || count > curveAsset().max_samples) return;
        const samples = Array.from({ length: count }, (_, index) => {
          const at = index / (count - 1) * (c.samples.length - 1), low = Math.floor(at), high = Math.ceil(at);
          return c.samples[low] + (c.samples[high] - c.samples[low]) * (at - low);
        });
        void commit(JSON.stringify({ ...c, samples }));
      }} /></label>
      <label>{i18n.t("curve.point")}<input type="number" min="0" max={curve().samples.length - 1} value={point()}
        onChange={e => { gesture.interrupt(); const value = Number(e.currentTarget.value); if (Number.isInteger(value)) setPoint(Math.max(0, Math.min(curve().samples.length - 1, value))); }} /></label>
      <label>{i18n.t("curve.value")}<input type="range" min={-curveAsset().max_level} max={curveAsset().max_level} step="0.001"
        disabled={lockedZero()} value={curve().samples[point()]} onKeyDown={event => { if (event.key === "Escape") { gesture.interrupt(); event.preventDefault(); event.stopPropagation(); } }} onPointerDown={e => gesture.pointerDown(e)} onPointerCancel={e => gesture.pointerCancel(e)}
        onInput={e => gesture.input(Number(e.currentTarget.value))} onChange={e => gesture.change(Number(e.currentTarget.value))} /></label>
      <output>{curve().samples[point()].toFixed(3)}</output>
    </div>
    {error() && <p role="alert">{i18n.t("curve.invalid")}</p>}
  </section>;
}
