/**
 * Compact control-link editor: one row per fan-out target under each control.
 *
 * The learn arm and the last-tweaked memory live in the store, so the synth
 * editor and the plugin panel show the same record and the same shortcut.
 */
import { For, Show, createEffect, createMemo, createSignal } from "solid-js";

import { automationLabel } from "../utils/automationTarget";
import { formulasFor, formulaInfo, linkSourceLabel } from "../utils/controlLinkFormula";
import {
  bindLastTweaked,
  cancelLearn,
  controlLinks,
  lastTweaked,
  learnFromMidiActivity,
  learnLastTweaked,
  learning,
  removeControlTarget,
  updateControlTarget,
} from "../stores/controlLinks";
import { i18n } from "../stores/i18n";
import { midiStore, type MidiActivity } from "../stores/midiLive";
import type { ControlLink, ControlLinkSource, ControlLinkTarget } from "../types/project";

const CONTROL_LINK_CSS = `
.control-link-editor {
  border-top: 1px solid var(--border); background: var(--bg-secondary);
  color: var(--text-secondary); font: 600 9px/1.2 var(--font-mono);
}
.control-link-bar { display: flex; align-items: center; gap: 6px; min-height: 22px; padding: 2px 10px; }
.control-link-toggle {
  padding: 0 4px; border: 0; background: none; color: var(--text-primary); cursor: pointer;
  font: 700 9px/1.2 var(--font-mono); letter-spacing: .06em;
}
.control-link-count { color: var(--text-secondary); }
.control-link-learn, .control-link-bind {
  padding: 1px 5px; border: 1px solid var(--border-strong); background: var(--bg-input);
  color: var(--text-primary); cursor: pointer; font: 650 9px/1.2 var(--font-mono);
}
.control-link-learn[aria-pressed="true"] { background: var(--accent); color: var(--bg-primary); border-color: var(--accent); }
.control-link-learn:disabled, .control-link-bind:disabled { opacity: .45; cursor: default; }
.control-link-armed { color: var(--accent); }
.control-link-last { margin-left: auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 38%; }
.control-link-list { padding: 2px 10px 6px; }
.control-link { padding: 2px 0 3px; border-top: 1px dashed var(--border); }
.control-link:first-child { border-top: 0; }
.control-link-head { display: flex; align-items: center; gap: 6px; color: var(--text-primary); }
.control-link-name { font-weight: 700; }
.control-link-source { color: var(--text-secondary); }
.control-link-bind { margin-left: auto; }
.control-link-row { display: grid; grid-template-columns: minmax(0, 1fr) 88px 54px 16px; gap: 6px; align-items: center; padding: 1px 0 1px 10px; }
.control-link-target { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-primary); }
.control-link-formula, .control-link-smoothing {
  height: 16px; min-width: 0; padding: 0 2px; border: 1px solid var(--border);
  background: var(--bg-input); color: var(--text-primary); font: 600 9px/1 var(--font-mono);
}
.control-link-remove {
  width: 14px; height: 14px; padding: 0; border: 0; background: none;
  color: var(--text-secondary); cursor: pointer; font: 700 11px/1 var(--font-mono);
}
.control-link-remove:hover { color: var(--danger, #e06c75); }
.control-link-empty { margin: 0; padding: 2px 10px 6px; color: var(--text-secondary); }
.control-link-error { margin: 0; padding: 2px 10px 4px; color: var(--danger, #e06c75); }
`;

function targetText(target: ControlLinkTarget): string {
  return `${target.track} · ${automationLabel(target.param)}`;
}

export function ControlLinkEditor(props: {
  /** Test/embed seam; defaults to the shared live-MIDI activity. */
  activity?: () => MidiActivity | null;
}) {
  const [open, setOpen] = createSignal(false);
  const [error, setError] = createSignal("");
  const links = createMemo<ControlLink[]>(() => controlLinks());
  const last = lastTweaked;

  const run = (action: Promise<unknown>) => {
    setError("");
    void action.catch((failure) => setError(String(failure)));
  };

  /** One live CC finishes an armed learn; the store writes exactly one row. */
  createEffect(() => {
    const activity = props.activity ? props.activity() : midiStore.activity();
    const event = activity?.last;
    if (!event) return;
    void learnFromMidiActivity(event).catch((failure) => setError(String(failure)));
  });

  const onLearn = () => {
    if (learning()) {
      cancelLearn();
      return;
    }
    if (!learnLastTweaked()) {
      setError(i18n.t("controlLink.noLast"));
      return;
    }
    setOpen(true);
  };

  const bind = (source: ControlLinkSource) => {
    run(bindLastTweaked(source));
  };

  return (
    <section class="control-link-editor" aria-label={i18n.t("controlLink.title")}>
      <style>{CONTROL_LINK_CSS}</style>
      <div class="control-link-bar">
        <button
          type="button"
          class="control-link-toggle"
          aria-expanded={open()}
          onClick={() => setOpen(!open())}
        >
          {i18n.t("controlLink.toggle")}
        </button>
        <span class="control-link-count">{links().length}</span>
        <button
          type="button"
          class="control-link-learn"
          aria-pressed={learning() !== null}
          disabled={!learning() && !last()}
          title={last() ? i18n.t("controlLink.learnHint") : i18n.t("controlLink.noLast")}
          onClick={onLearn}
        >
          {learning() ? i18n.t("controlLink.cancel") : i18n.t("controlLink.learn")}
        </button>
        <Show when={learning()}>
          <span class="control-link-armed" role="status">{i18n.t("controlLink.learnArmed")}</span>
        </Show>
        <Show when={last()}>
          {(entry) => (
            <span class="control-link-last" title={entry().label}>
              {i18n.t("controlLink.lastTweaked", { param: entry().label })}
            </span>
          )}
        </Show>
      </div>

      <Show when={open()}>
        <Show
          when={links().length > 0}
          fallback={<p class="control-link-empty">{i18n.t("controlLink.empty")}</p>}
        >
          <div class="control-link-list">
            <For each={links()}>
              {(link) => (
                <div class="control-link">
                  <div class="control-link-head">
                    <span class="control-link-name">{link.name}</span>
                    <span class="control-link-source">{linkSourceLabel(link.source)}</span>
                    <button
                      type="button"
                      class="control-link-bind"
                      disabled={!last()}
                      title={last() ? i18n.t("controlLink.bindLastHint") : i18n.t("controlLink.noLast")}
                      onClick={() => bind(link.source)}
                    >
                      {i18n.t("controlLink.bindLast")}
                    </button>
                  </div>
                  <For each={link.targets}>
                    {(target) => (
                      <div class="control-link-row">
                        <span class="control-link-target" title={targetText(target)}>
                          {targetText(target)}
                        </span>
                        <select
                          class="control-link-formula"
                          aria-label={i18n.t("controlLink.formula")}
                          value={target.formula}
                          title={formulaInfo(target.formula).mapping}
                          onChange={(event) => {
                            const formula = event.currentTarget.value as ControlLinkTarget["formula"];
                            run(updateControlTarget(link.source, target, formula, target.smoothing_ms));
                          }}
                        >
                          <For each={formulasFor(link.source)}>
                            {(formula) => (
                              <option value={formula.value} title={formula.mapping}>
                                {formula.label}
                              </option>
                            )}
                          </For>
                        </select>
                        <input
                          class="control-link-smoothing"
                          type="number"
                          min={0}
                          max={60000}
                          step={1}
                          value={target.smoothing_ms}
                          aria-label={i18n.t("controlLink.smoothing")}
                          title={i18n.t("controlLink.smoothingHint", {
                            value: String(target.smoothing_ms),
                          })}
                          onChange={(event) => {
                            const smoothing = Number(event.currentTarget.value);
                            run(
                              updateControlTarget(
                                link.source,
                                target,
                                target.formula,
                                Number.isFinite(smoothing) ? smoothing : 0,
                              ),
                            );
                          }}
                        />
                        <button
                          type="button"
                          class="control-link-remove"
                          aria-label={i18n.t("controlLink.remove")}
                          title={i18n.t("controlLink.remove")}
                          onClick={() => run(removeControlTarget(link.source, target.track, target.param))}
                        >
                          ×
                        </button>
                      </div>
                    )}
                  </For>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Show>

      <Show when={error()}>
        <p class="control-link-error" role="alert">{error()}</p>
      </Show>
    </section>
  );
}
