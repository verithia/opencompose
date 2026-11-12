/**
 * BusActions — node-level card for the selected bus: delete it directly when
 * nothing references it, or ask for an explicit re-route confirmation in a
 * dialog when tracks, sends or other buses still point at it.
 */
import { Show, createSignal } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import {
  busReferences,
  busTotalReferences,
  describeBusReferences,
  type BusReferences,
} from "../utils/mixerRouting";

export function BusActions(props: {
  busName: string;
  /** Called after a successful delete with the node that should get focus. */
  onDeleted: (focusNodeId: string) => void;
}) {
  const [pending, setPending] = createSignal<BusReferences | null>(null);
  let deleteButton: HTMLButtonElement | undefined;

  const requestDelete = () => {
    const refs = busReferences(store.project, props.busName);
    if (busTotalReferences(refs) === 0) {
      void store.removeBus(props.busName).then((removed) => {
        if (removed) props.onDeleted("master");
      });
      return;
    }
    setPending(refs);
  };

  const cancel = () => {
    setPending(null);
    deleteButton?.focus();
  };

  const confirm = async () => {
    if (!await store.removeBusReroute(props.busName)) return;
    setPending(null);
    props.onDeleted("master");
  };

  return (
    <div class="routing-node-actions" role="group" aria-label={i18n.t("routing.busActions")}>
      <button
        ref={deleteButton}
        class="routing-btn routing-danger"
        aria-label={i18n.t("routing.deleteBusLabel")}
        onClick={requestDelete}
      >{i18n.t("routing.deleteBus")}</button>

      <Show when={pending()}>
        {(refs) => (
          <div class="routing-dialog-backdrop" onClick={cancel}>
            <div
              class="routing-dialog"
              role="alertdialog"
              aria-modal="true"
              aria-label={i18n.t("routing.deleteInUseTitle")}
              onClick={(event) => event.stopPropagation()}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  cancel();
                }
              }}
            >
              <div class="routing-dialog-title">{i18n.t("routing.deleteInUseTitle")}</div>
              <p>
                {i18n.t("routing.deleteInUseBody", {
                  name: props.busName,
                  refs: describeBusReferences(refs(), i18n.t),
                })}
              </p>
              <div class="routing-dialog-actions">
                <button
                  class="routing-btn"
                  aria-label={i18n.t("routing.cancelDeleteLabel")}
                  onClick={cancel}
                >{i18n.t("routing.cancel")}</button>
                <button
                  ref={(element) => {
                    queueMicrotask(() => element.focus());
                  }}
                  class="routing-btn routing-danger"
                  aria-label={i18n.t("routing.rerouteDeleteLabel")}
                  onClick={() => void confirm()}
                >{i18n.t("routing.rerouteDelete")}</button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
