import { createSignal } from "solid-js";

const [sampleCredentialsChanged, setChanged] = createSignal(0);

export { sampleCredentialsChanged };

export function notifySampleCredentialsChanged() {
  setChanged((revision) => revision + 1);
}
