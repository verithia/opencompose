import { beginPresetMacroPreview, endPresetMacroPreview, updatePresetMacroPreview } from "./effectsApi";
import type { EffectTouchToken } from "./effectsApi";
import type { ParameterRequestOwner } from "./parameterAdmission";
import type { ParameterGesturePreview } from "../utils/parameterGesture";

/** Keep only the latest pointer value while a guarded request is in flight. */
export function capturePresetMacroPreview(
  preset: string, index: number, owner: ParameterRequestOwner,
): ParameterGesturePreview | null {
  if (!owner()) return null;
  let token: EffectTouchToken | null = null;
  let finished: boolean | null = null;
  let pending: number | null = null;
  let inFlight = false;
  let timer: number | undefined;

  const finish = (committed: boolean) => {
    if (finished === null) finished = committed;
    if (timer !== undefined) window.clearTimeout(timer);
    timer = undefined;
    pending = null;
    if (token) {
      const closing = token;
      token = null;
      void endPresetMacroPreview(preset, index, closing, finished).catch(() => {});
    }
  };
  const schedule = () => {
    if (finished !== null || !token || inFlight || timer !== undefined || pending === null) return;
    timer = window.setTimeout(() => {
      timer = undefined;
      if (finished !== null || !token || pending === null) return;
      if (!owner()) { finish(false); return; }
      const value = pending;
      pending = null;
      inFlight = true;
      void updatePresetMacroPreview(preset, index, value, token)
        .then(accepted => { if (!accepted) finish(false); }, () => finish(false))
        .finally(() => {
          inFlight = false;
          schedule();
        });
    }, 16);
  };
  try {
    void beginPresetMacroPreview(preset, index, owner.requireStamp()).then(granted => {
      if (!granted) { finish(false); return; }
      token = granted;
      if (finished !== null || !owner()) finish(false);
      else schedule();
    }, () => finish(false));
  } catch { return null; }
  return {
    update(value) {
      if (finished !== null) return;
      pending = value;
      schedule();
    },
    finish,
  };
}
