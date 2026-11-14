import { For, Show, createMemo } from "solid-js";
import { auditionLabel, formatBytes, formatDuration } from "../ai/media";
import type { ChatMessage } from "../ai/transcript";
import { openSavedAuditionRender, previewSavedAudition } from "../stores/api";
import { i18n } from "../stores/i18n";
import { InlineAudioPreview } from "./InlineAudioPreview";
import { LazyDetails } from "./LazyDetails";

/** Listening deliveries stay one line until the operator opens their controls. */
export function AiAudition(props: { message: ChatMessage }) {
  const audio = createMemo(() => props.message.media?.filter(item => item.kind === "audio") ?? []);
  const label = createMemo(() => {
    const first = audio()[0];
    const range = first && auditionLabel(first, props.message.content, props.message.messageMeta);
    return range ? i18n.t(range.key, range.params) : i18n.t("ai.media.savedAudition");
  });
  return (
    <LazyDetails class="ai-audition-detail" open={false} summary={
      <>
        <span class="ai-audition-label">{label()}</span>
        <Show when={audio()[0]}>{item => <span class="ai-audition-duration">{formatDuration(item().duration_secs)}</span>}</Show>
        <Show when={props.message.auditionRenderUnavailable || audio().some(item => item.missing)}>
          <span class="ai-audition-missing" aria-label={i18n.t("ai.media.missingState")}>!</span>
        </Show>
      </>
    }>
      <div class="ai-audition-body">
        <For each={audio()}>{attachment => (
          <div class="ai-audition-file">
            <span class="ai-audition-metadata" title={attachment.path}>
              {attachment.missing ? i18n.t("ai.media.missingState")
                : `${attachment.mime} · ${formatDuration(attachment.duration_secs) ?? i18n.t("ai.media.unknownDuration")} · ${formatBytes(attachment.bytes)}`}
            </span>
            <Show when={!attachment.missing}>
              <InlineAudioPreview label={i18n.t("ai.media.loadAudition")} resetKey={attachment.path}
                load={() => previewSavedAudition(attachment.path)} />
            </Show>
          </div>
        )}</For>
        <Show when={props.message.auditionRenderPath}>{path => (
          <InlineAudioPreview label={i18n.t("ai.media.loadAuditionRender")} resetKey={path()}
            load={() => openSavedAuditionRender(path())} />
        )}</Show>
        <Show when={props.message.auditionRenderUnavailable}>
          <span class="ai-session-warning" role="status">{i18n.t(audio().some(item => !item.missing)
            ? "ai.media.auditionRenderUnavailable" : "ai.media.auditionRenderMissing")}</span>
        </Show>
        <Show when={!audio().length && !props.message.auditionRenderPath && !props.message.auditionRenderUnavailable}>
          <span class="ai-audition-metadata">{i18n.t("ai.media.legacyAudition")}</span>
        </Show>
      </div>
    </LazyDetails>
  );
}
