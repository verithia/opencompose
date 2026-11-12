import { For, Show, createMemo, createSignal, createEffect } from "solid-js";
import type { PerformanceEvent, Track } from "../types/project";
import { i18n } from "../stores/i18n";
import "./PerformanceEditor.css";

type Edit = (trackIndex: number, trackName: string, eventIndex: number,
  expected: PerformanceEvent, replacement: PerformanceEvent) => Promise<unknown>;

/** Ordered channel commands, not inferred piano-roll note pairs. */
export function PerformanceEditor(props: { track: Track; trackIndex: number; onEdit: Edit }) {
  const [page, setPage] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const events = () => props.track.performance?.events ?? [];
  const pages = () => Math.max(1, Math.ceil(events().length / 64));
  createEffect(() => { props.trackIndex; setPage(0); setError(""); });
  const rows = createMemo(() => events().slice(page() * 64, (page() + 1) * 64));

  function Field(p: { event: PerformanceEvent; index: number; field: string; value: number; message?: boolean }) {
    let expected = JSON.parse(JSON.stringify(p.event)) as PerformanceEvent;
    return <input aria-label={i18n.t("performance.fieldLabel", { index: p.index + 1, field: p.field })} value={p.value}
      inputmode={p.field === "quarter" ? "decimal" : "numeric"} disabled={busy()}
      onFocus={() => { expected = JSON.parse(JSON.stringify(p.event)) as PerformanceEvent; }}
      onChange={async (e) => {
        const input = e.currentTarget;
        const raw = input.value.trim();
        const value = Number(raw);
        if (!raw || !Number.isFinite(value) || (p.field !== "quarter" && !Number.isInteger(value))) {
          setError(i18n.t("performance.invalidValue"));
          input.value = String(p.value); return;
        }
        const replacement = JSON.parse(JSON.stringify(expected)) as PerformanceEvent;
        if (p.message) Object.assign(replacement.message, { [p.field]: value });
        else Object.assign(replacement, { [p.field]: value });
        setBusy(true); setError("");
        try { await props.onEdit(props.trackIndex, props.track.name, p.index, expected, replacement); }
        catch (error) { setError(String(error)); input.value = String(p.value); }
        finally { setBusy(false); }
      }} />;
  }

  return <section class="performance-editor" aria-label={i18n.t("performance.editorLabel")}>
    <header><strong>{i18n.t("performance.title", { name: props.track.name })}</strong>
      <span>{i18n.t("performance.subtitle", { count: events().length })}</span></header>
    <p>{i18n.t("performance.help")}</p>
    <Show when={error()}><p class="selectable" role="alert">{error()}</p></Show>
    <div class="performance-table-scroll"><table>
      <thead><tr><th scope="col">#</th><th scope="col">{i18n.t("performance.quarter")}</th><th scope="col">{i18n.t("midi.import.channel")}</th>
        <th scope="col">{i18n.t("performance.command")}</th><th scope="col">{i18n.t("performance.data")}</th></tr></thead>
      <tbody><For each={rows()}>{(event, offset) => {
        const index = () => page() * 64 + offset();
        return <tr><td>{index() + 1}</td>
          <td><Field event={event} index={index()} field="quarter" value={event.quarter} /></td>
          <td><Field event={event} index={index()} field="channel" value={event.channel} /></td>
          <td>{event.message.type}</td><td><For each={Object.entries(event.message).filter(([key]) => key !== "type")}>{([field, value]) =>
            <label>{field} <Field event={event} index={index()} field={field} value={Number(value)} message /></label>
          }</For></td></tr>;
      }}</For></tbody>
    </table></div>
    <footer><button disabled={busy() || page() === 0} onClick={() => setPage(page() - 1)}>{i18n.t("performance.previous")}</button>
      <span>{i18n.t("performance.page", { page: page() + 1, pages: pages() })}</span>
      <button disabled={busy() || page() + 1 >= pages()} onClick={() => setPage(page() + 1)}>{i18n.t("performance.next")}</button></footer>
  </section>;
}
