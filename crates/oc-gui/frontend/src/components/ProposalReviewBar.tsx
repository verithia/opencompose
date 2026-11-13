import { Show } from "solid-js";
import { i18n } from "../stores/i18n";
import { previewStore } from "../stores/preview";

/**
 * The bar of a preview the backend holds: the two sides on the left, the
 * decision on the right.
 */
export function ProposalReviewBar(props: {
  applying: boolean;
  previewPath: string;
  onPreviewAccept: () => Promise<void>;
  onPreviewDecline: () => Promise<void>;
}) {
  return (
    <>
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
