import { For, Show } from "solid-js";
import { clipInformation, selectedClipKey } from "../stores/clipInfo";
import { i18n } from "../stores/i18n";
import { PianoRoll } from "./PianoRoll";

/** Inspect the actual placement and sound source, rather than repeat the clip list. */
export function ClipDetailView() {
  const info = () => { const key = selectedClipKey(); return key ? clipInformation(key) : null; };
  return <div class="clip-detail-view" classList={{ "with-notes": info()?.kind === "pattern" }}>
    <section class="clip-inspector" aria-label={i18n.t("clip.info.inspect")}>
      <Show when={info()} fallback={<p>{i18n.t("clip.info.select")}</p>}>{value => <>
        <h2>{value().title}</h2>
        <dl class="context-menu-info"><For each={value().rows}>{row => <div><dt>{row.label}</dt><dd classList={{ "context-menu-path": row.path }}>{row.value}</dd></div>}</For></dl>
      </>}</Show>
    </section>
    <Show when={info()?.kind === "pattern"}><div class="clip-detail-notes"><PianoRoll /></div></Show>
  </div>;
}
