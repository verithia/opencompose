import { Show } from "solid-js";
import { i18n } from "../stores/i18n";
import { previewStore } from "../stores/preview";
import { useTooltip } from "./Tooltip";

export function ProposalReviewBar(props: {
  pendingPath: string | undefined;
  applying: boolean;
  loading: boolean;
  previewChangeText: string | null;
  previewPath: string;
  onPreview: () => Promise<void>;
  onAccept: () => Promise<void>;
  onReject: () => Promise<void>;
  onPreviewAccept: () => Promise<void>;
  onPreviewDecline: () => Promise<void>;
}) {
  return (
    <>
      {/* Accept/Reject bar. A preview replaces it: the same proposal now has a
          document to listen to, so the decision belongs to the preview bar. */}
      <Show when={props.pendingPath !== undefined && !previewStore.open()}>
        <div class="ai-accept-bar">
          <span class="selectable" {...useTooltip(() => props.pendingPath)}>{props.pendingPath}</span>
          {/* A turn waiting on this decision is paused, not busy: the buttons stay live. */}
          <button
            class="ai-preview-open"
            disabled={props.applying || props.loading || previewStore.busy()}
            onClick={props.onPreview}
          >{i18n.t("ai.preview.start")}</button>
          <button
            class="ai-accept-btn"
            disabled={props.applying || props.loading}
            onClick={props.onAccept}
          >{i18n.t("ai.edit.accept")}</button>
          <button
            class="ai-reject-btn"
            disabled={props.applying || props.loading}
            onClick={props.onReject}
          >{i18n.t("ai.edit.reject")}</button>
        </div>
      </Show>

      {/* Preview bar: the two sides on the left, the decision on the right. */}
      <Show when={previewStore.open()}>
        <div class="ai-preview-bar" role="group" aria-label={i18n.t("ai.preview.start")}>
          <div class="ai-preview-sides">
            <button
              class="ai-preview-side ai-preview-apply"
              classList={{ active: previewStore.side() === "proposal" }}
              disabled={previewStore.busy()}
              onClick={() => void previewStore.show("proposal")}
            >{i18n.t("ai.preview.apply")}</button>
            <button
              class="ai-preview-side ai-preview-rewind"
              classList={{ active: previewStore.side() === "base" }}
              disabled={previewStore.busy()}
              onClick={() => void previewStore.show("base")}
            >{i18n.t("ai.preview.rewind")}</button>
          </div>
          {/* The change line sits above the side hint in one text column, so a
              long diff summary wraps instead of pushing the buttons out. */}
          <div class="ai-preview-text">
            <Show when={props.previewChangeText}>
              <span class="ai-preview-change selectable">{props.previewChangeText}</span>
            </Show>
            <span class="ai-preview-hint selectable">
              {i18n.t(
                previewStore.side() === "proposal" ? "ai.preview.hintProposal" : "ai.preview.hintBase",
                { path: props.previewPath },
              )}
            </span>
          </div>
          <div class="ai-preview-actions">
            <button
              class="ai-preview-accept"
              disabled={previewStore.busy() || props.applying}
              onClick={() => void props.onPreviewAccept()}
            >{i18n.t("ai.preview.accept")}</button>
            <button
              class="ai-preview-decline"
              disabled={previewStore.busy() || props.applying}
              onClick={() => void props.onPreviewDecline()}
            >{i18n.t("ai.preview.decline")}</button>
          </div>
        </div>
      </Show>
    </>
  );
}
