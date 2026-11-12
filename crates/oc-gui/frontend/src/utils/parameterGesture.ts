import {
  automationWriteArmed,
  closeAutomationWrite,
  openAutomationWrite,
  sampleAutomationWrite,
  type AutomationWriteHandle,
  type AutomationWriteSurface,
} from "../stores/automationWrite";
import { isSecondaryClick } from "../stores/gesture";

interface ParameterGestureOptions {
  value(): number;
  draft(value: number): void;
  clearDraft(): void;
  commit(value: number): void;
  /** Capture ordinary edit admission before closing another interaction. */
  captureCommit?(): (value: number) => void;
  surface(): AutomationWriteSurface | null;
  quarter(): number;
  opened?(handle: AutomationWriteHandle): void;
  closed?(handle: AutomationWriteHandle): void;
}
interface Interaction {
  kind: "pointer" | "implicit" | "keyboard";
  pointerId: number;
  started: number;
  handle: AutomationWriteHandle | null;
  value: number;
  changed: boolean;
  ended: boolean;
  commit(value: number): void;
  removeListeners(): void;
}

/** A DOM change has no pointer identity. Pointer edits therefore finish from
 * their captured release/cancel owner; subsequent change events are ignored.
 * Input/change without a pointer remains available to accessibility clients. */
export function createParameterGesture(options: ParameterGestureOptions) {
  let current: Interaction | null = null;
  let opening: object | null = null;
  let disposed = false;
  const owns = (interaction: Interaction) => !disposed && current === interaction && !interaction.ended;
  const closeHandle = (interaction: Interaction) => {
    const handle = interaction.handle;
    interaction.handle = null;
    if (!handle) return;
    void closeAutomationWrite(handle);
    options.closed?.(handle);
  };
  const sample = (interaction: Interaction) => {
    const handle = interaction.handle;
    if (!handle || !owns(interaction)) return;
    const quarter = options.quarter();
    if (owns(interaction) && interaction.handle === handle) {
      sampleAutomationWrite(handle, quarter, interaction.value);
    }
  };
  const finish = (interaction: Interaction, canceled = false) => {
    if (interaction.ended) return;
    const active = owns(interaction);
    // Sampling can call a surface accessor, so it may dispose or supersede us.
    if (!canceled && active && interaction.handle) sample(interaction);
    if (interaction.ended) return;
    interaction.ended = true;
    interaction.removeListeners();
    const handle = interaction.handle;
    closeHandle(interaction);
    if (disposed || current !== interaction) return;
    if (canceled || handle) options.clearDraft();
    else if (interaction.changed) interaction.commit(interaction.value);
  };
  const open = (kind: Interaction["kind"], pointerId = 0, started = 0): Interaction | null => {
    if (disposed) return null;
    const owner = {};
    opening = owner;
    const admitted = () => !disposed && opening === owner;
    const commit = options.captureCommit?.() ?? options.commit;
    if (!admitted()) return null;
    const surface = automationWriteArmed() ? options.surface() : null;
    if (!admitted()) return null;
    const value = options.value();
    if (!admitted()) return null;
    // Both possible write paths already belong to this input's original source;
    // closing the old draft may synchronously retarget or unmount its control.
    if (current && !current.ended) finish(current, true);
    if (!admitted()) return null;
    const interaction: Interaction = {
      kind, pointerId, started, handle: null, value, changed: false,
      ended: false, commit, removeListeners: () => {},
    };
    current = interaction;
    interaction.handle = surface ? openAutomationWrite(surface) : null;
    const accepted = () => admitted() && owns(interaction);
    const abandon = () => {
      if (!interaction.ended) finish(interaction, true);
      // Disposal during openAutomationWrite can precede handle publication.
      // Even an already-ended interaction must release that newly returned handle.
      closeHandle(interaction);
      return null;
    };
    if (!accepted()) return abandon();
    if (interaction.handle) options.opened?.(interaction.handle);
    if (!accepted()) return abandon();
    return interaction;
  };
  const input = (value: number) => {
    if (disposed) return;
    // Explicit input can begin a fresh AX edit; change alone cannot revive an
    // ended pointer/keyboard stream or a disposed component.
    const interaction = current && !current.ended ? current : open("implicit");
    if (!interaction || !owns(interaction)) return;
    interaction.value = value;
    interaction.changed = true;
    options.draft(value);
    if (owns(interaction)) sample(interaction);
    return interaction;
  };
  const matches = (interaction: Interaction, event: PointerEvent) => {
    if (!owns(interaction) || interaction.pointerId !== (event.pointerId ?? 0)) return false;
    // Identity protects captured old handlers even when browsers reuse pointer
    // IDs or expose zero timestamps; time is only an additional filter.
    return !(interaction.started > 0 && event.timeStamp > 0 && event.timeStamp < interaction.started);
  };
  const pointerDown = (event: PointerEvent) => {
    if (disposed || (event.button ?? 0) !== 0 || isSecondaryClick(event)) return false;
    const interaction = open("pointer", event.pointerId ?? 0, event.timeStamp);
    if (!interaction || !owns(interaction)) return false;
    const up = (release: PointerEvent) => { if (matches(interaction, release)) finish(interaction); };
    const cancel = (release: PointerEvent) => { if (matches(interaction, release)) finish(interaction, true); };
    const blur = () => { if (owns(interaction)) finish(interaction, true); };
    interaction.removeListeners = () => {
      window.removeEventListener("pointerup", up, true);
      window.removeEventListener("pointercancel", cancel, true);
      window.removeEventListener("blur", blur);
    };
    window.addEventListener("pointerup", up, true);
    if (!owns(interaction)) { interaction.removeListeners(); return false; }
    window.addEventListener("pointercancel", cancel, true);
    if (!owns(interaction)) { interaction.removeListeners(); return false; }
    window.addEventListener("blur", blur);
    if (!owns(interaction)) { interaction.removeListeners(); return false; }
    return true;
  };
  const discrete = (value: number) => {
    if (disposed) return;
    const interaction = open("keyboard");
    if (!interaction || !owns(interaction)) return;
    interaction.value = value;
    interaction.changed = true;
    options.draft(value);
    if (owns(interaction)) finish(interaction);
  };
  return {
    pointerDown,
    input,
    change(value: number) {
      if (disposed || current?.kind === "pointer" || current?.kind === "keyboard" || current?.ended) return;
      const interaction = input(value);
      if (interaction && owns(interaction)) finish(interaction);
    },
    pointerCancel(event: PointerEvent) {
      if (disposed || !current || current.ended) return;
      if (current.kind === "pointer" && !matches(current, event)) return;
      finish(current, true);
    },
    discrete,
    reset(value: number) {
      if (disposed) return;
      if (!current || current.ended || !current.handle) { discrete(value); return; }
      const interaction = current;
      interaction.value = value;
      interaction.changed = true;
      options.draft(value);
      if (owns(interaction)) finish(interaction);
    },
    interrupt() {
      if (disposed) return;
      opening = null;
      if (current) finish(current, true);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      opening = null;
      if (current) finish(current, true);
    },
  };
}
