/**
 * The bounce/consolidate prompt.
 *
 * A bounce asks for a bar range and a target; a consolidate shows the span of
 * the selected audio clips, because that span is what the command renders. The
 * backend validates the range again, so this dialog only keeps the common
 * mistakes out of the round trip.
 */
import { Show } from "solid-js";
import { i18n } from "../stores/i18n";
import { bounceStore, rangeCutsNote, rangeFailureKey } from "../stores/bounce";
import { store } from "../stores/project";

export function BounceDialog() {
  return (
    <Show when={bounceStore.request()}>
      {(request) => {
        const localFailure = () =>
          request().kind === "bounce"
            ? rangeFailureKey(store.project.properties, request().from, request().to)
            : null;
        const cutsNote = () =>
          request().kind === "bounce" &&
          rangeCutsNote(store.project, request().track, request().to);
        const title = () =>
          request().kind === "bounce" ? i18n.t("bounce.title") : i18n.t("bounce.consolidateTitle");
        const confirmLabel = () =>
          request().kind === "bounce" ? i18n.t("bounce.confirm") : i18n.t("bounce.consolidate");
        const pendingLabel = () =>
          request().kind === "bounce"
            ? i18n.t("bounce.pendingBounce")
            : i18n.t("bounce.pendingConsolidate");
        return (
          <div
            class="text-prompt-overlay"
            onPointerDown={(event) => {
              if (event.target === event.currentTarget && !bounceStore.pending()) {
                bounceStore.close();
              }
            }}
          >
            <form
              class="text-prompt bounce-prompt"
              role="dialog"
              aria-modal="true"
              aria-label={title()}
              onSubmit={(event) => {
                event.preventDefault();
                void bounceStore.confirm();
              }}
              onKeyDown={(event) => {
                if (event.key === "Escape" && !bounceStore.pending()) {
                  event.preventDefault();
                  event.stopPropagation();
                  bounceStore.close();
                }
              }}
            >
              <strong class="bounce-title">{title()}</strong>
              <label class="text-prompt-label">
                <span>{i18n.t("bounce.from")}</span>
                <input
                  class="text-prompt-input"
                  aria-label={i18n.t("bounce.from")}
                  value={request().from}
                  readOnly={request().kind === "consolidate"}
                  onInput={(event) =>
                    bounceStore.setRange(event.currentTarget.value, request().to)
                  }
                />
              </label>
              <label class="text-prompt-label">
                <span>{i18n.t("bounce.to")}</span>
                <input
                  class="text-prompt-input"
                  aria-label={i18n.t("bounce.to")}
                  value={request().to}
                  readOnly={request().kind === "consolidate"}
                  onInput={(event) =>
                    bounceStore.setRange(request().from, event.currentTarget.value)
                  }
                />
              </label>
              <Show when={request().kind === "bounce"}>
                <div class="bounce-mode" role="radiogroup" aria-label={i18n.t("bounce.mode")}>
                  <label>
                    <input
                      type="radio"
                      name="bounce-mode"
                      checked={request().mode === "new_track"}
                      onChange={() => bounceStore.setMode("new_track")}
                    />
                    {i18n.t("bounce.newTrack")}
                  </label>
                  <label>
                    <input
                      type="radio"
                      name="bounce-mode"
                      checked={request().mode === "in_place"}
                      onChange={() => bounceStore.setMode("in_place")}
                    />
                    {i18n.t("bounce.inPlace")}
                  </label>
                </div>
                <p class="bounce-hint">
                  {request().mode === "new_track"
                    ? i18n.t("bounce.newTrackHint")
                    : i18n.t("bounce.inPlaceHint")}
                </p>
                <Show when={cutsNote()}>
                  <p class="bounce-warning selectable">{i18n.t("bounce.cutsNote")}</p>
                  <div
                    class="bounce-mode"
                    role="radiogroup"
                    aria-label={i18n.t("bounce.tail")}
                  >
                    <label>
                      <input
                        type="radio"
                        name="bounce-tail"
                        checked={request().tail === "include_release"}
                        onChange={() => bounceStore.setTail("include_release")}
                      />
                      {i18n.t("bounce.tailInclude")}
                    </label>
                    <label>
                      <input
                        type="radio"
                        name="bounce-tail"
                        checked={request().tail === "cut_exactly"}
                        onChange={() => bounceStore.setTail("cut_exactly")}
                      />
                      {i18n.t("bounce.tailCut")}
                    </label>
                  </div>
                </Show>
              </Show>
              <Show when={request().kind === "consolidate"}>
                <p class="bounce-hint">{i18n.t("bounce.selectionSpan")}</p>
              </Show>
              <p class="bounce-hint">{i18n.t("bounce.rangeHint")}</p>
              <Show when={localFailure()}>
                {(key) => <p class="bounce-failure selectable">{i18n.t(key())}</p>}
              </Show>
              <Show when={bounceStore.failure()}>
                {(message) => <p class="bounce-failure selectable">{message()}</p>}
              </Show>
              <div class="text-prompt-actions">
                <button
                  type="button"
                  class="text-prompt-btn"
                  disabled={bounceStore.pending()}
                  onClick={() => bounceStore.close()}
                >
                  {i18n.t("dialog.cancel")}
                </button>
                <button
                  type="submit"
                  class="text-prompt-btn primary"
                  disabled={bounceStore.pending() || localFailure() !== null}
                >
                  {bounceStore.pending() ? pendingLabel() : confirmLabel()}
                </button>
              </div>
            </form>
          </div>
        );
      }}
    </Show>
  );
}
