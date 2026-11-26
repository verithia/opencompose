/** One owner/index request connects Timeline gain previews to the rack editor. */
import { createSignal } from "solid-js";
import { panelStore } from "./panels";
const [request, setRequest] = createSignal<{
  target: string;
  index: number;
  pointIndex?: number;
} | null>(null);
export const gainAutomationUi = {
  request,
  clear() {
    setRequest(null);
  },
  open(target: string, index: number, pointIndex?: number) {
    setRequest({ target, index, pointIndex });
    panelStore.openBottom("mixer");
  },
};
