/**
 * RoutingToolbar — the project-level band above the routing graph.
 *
 * It only creates bus nodes. Node-level actions live next to the channel
 * controls in the inspector, and connection editing has its own inspector
 * card, so this row never changes meaning when a node or an edge is selected.
 */
import { createSignal } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { nextBusName } from "../utils/mixerRouting";

export function RoutingToolbar(props: {
  onSelectNode: (id: string | null) => void;
}) {
  const [busName, setBusName] = createSignal("");

  async function createBus() {
    const name = busName().trim() || nextBusName(store.project.buses);
    const created = await store.addBus(name, ["master"]);
    if (!created) return;
    setBusName("");
    props.onSelectNode(`bus:${created}`);
  }

  return (
    <div class="routing-toolbar" role="toolbar" aria-label={i18n.t("routing.toolbarLabel")}>
      <div class="routing-section">
        <span class="routing-section-title">{i18n.t("routing.project")}</span>
        <input
          aria-label={i18n.t("routing.newBusName")}
          placeholder={nextBusName(store.project.buses)}
          value={busName()}
          onInput={(event) => setBusName(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void createBus();
            }
          }}
        />
        <button
          class="routing-btn"
          aria-label={i18n.t("routing.createBus")}
          onClick={() => void createBus()}
        >{i18n.t("routing.addBusButton")}</button>
      </div>
      <span class="routing-hint">{i18n.t("routing.hint")}</span>
    </div>
  );
}
