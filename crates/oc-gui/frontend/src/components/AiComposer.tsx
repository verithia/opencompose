import { For, Show } from "solid-js";
import type { ProjectReference } from "../ai/references";
import { i18n, type I18nKey } from "../stores/i18n";
import { useTooltip } from "./Tooltip";

export function AiComposer(props: {
  blockedNotice: I18nKey | null;
  goal: string;
  onClearGoal: () => void;
  commands: readonly { name: string; summary: I18nKey }[];
  commandIndex: number;
  onCommandHover: (index: number) => void;
  onCommand: (name: string) => Promise<void>;
  references: readonly ProjectReference[];
  referenceIndex: number;
  onReferenceHover: (index: number) => void;
  onReference: (reference: ProjectReference) => void;
  scope: string | null;
  onClearScope: () => void;
  attachHint: string;
  attachDisabled: boolean;
  onAttach: () => Promise<void>;
  loading: boolean;
  input: string;
  inputDisabled: boolean;
  inputRef: (element: HTMLTextAreaElement) => void;
  onInput: (element: HTMLTextAreaElement) => void;
  onInputKey: (event: KeyboardEvent) => void;
  sendAction: "send" | "cancel";
  sendLabel: string;
  sendTitle: string;
  sendDisabled: boolean;
  onSend: () => Promise<void>;
  onCancel: () => Promise<void>;
}) {
  const attachTooltip = useTooltip(() => props.attachHint);
  return (
    <>
      {/* Composer: one row, then the session meta line and any save notice. */}
      <Show when={props.blockedNotice}>
        <div class="ai-send-blocked"><small>{i18n.t(props.blockedNotice!)}</small></div>
      </Show>
      <Show when={props.goal}>
        <div class="ai-goal-chip">
          <span class="ai-goal-label">{i18n.t("ai.goal.label")}</span>
          <span class="ai-goal-text selectable">{props.goal}</span>
          <button
            type="button"
            class="ai-goal-clear"
            title={i18n.t("ai.goal.clear")}
            aria-label={i18n.t("ai.goal.clear")}
            onClick={props.onClearGoal}
          >×</button>
        </div>
      </Show>

      <Show when={props.commands.length > 0}>
        <div class="ai-command-palette" role="listbox" aria-label={i18n.t("ai.command.title")}>
          <For each={props.commands}>
            {(command, index) => (
              <button
                type="button"
                role="option"
                aria-selected={props.commandIndex === index()}
                class="ai-command-row"
                classList={{ active: props.commandIndex === index() }}
                onMouseEnter={() => props.onCommandHover(index())}
                onClick={() => void props.onCommand(command.name)}
              >
                <span class="ai-command-name">/{command.name}</span>
                <span class="ai-command-summary">{i18n.t(command.summary)}</span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={props.scope}>
        <div class="ai-scope-chip" title={i18n.t("ai.reference.scope")}>
          <span class="ai-scope-path">@ {props.scope}</span>
          <button
            type="button"
            class="ai-scope-clear"
            aria-label={i18n.t("ai.reference.clear")}
            title={i18n.t("ai.reference.clear")}
            onClick={props.onClearScope}
          >×</button>
        </div>
      </Show>

      <Show when={props.references.length > 0}>
        <div class="ai-reference-palette" role="listbox" aria-label={i18n.t("ai.reference.title")}>
          <For each={props.references}>
            {(reference, index) => (
              <button
                type="button"
                role="option"
                aria-selected={props.referenceIndex === index()}
                class="ai-reference-row"
                classList={{ active: props.referenceIndex === index() }}
                onMouseEnter={() => props.onReferenceHover(index())}
                onClick={() => props.onReference(reference)}
              >
                <span class="ai-reference-path">{reference.path}</span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <div class="ai-input-bar">
        <button
          class="ai-attach-btn"
          aria-label={i18n.t("ai.media.attach")}
          onClick={() => void props.onAttach()}
          disabled={props.attachDisabled}
          {...attachTooltip}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path
              d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
        {/* A textarea, so a pasted multi-line prompt keeps its lines and
            Shift+Enter inserts one instead of doing nothing. */}
        <textarea
          class="ai-input"
          rows={1}
          value={props.input}
          onInput={(e) => props.onInput(e.currentTarget)}
          onKeyDown={props.onInputKey}
          placeholder={props.loading ? i18n.t("ai.queue.placeholder") : i18n.t("ai.input.placeholder")}
          disabled={props.inputDisabled}
          ref={props.inputRef}
        />
        <button
          class="ai-send-btn"
          classList={{ cancel: props.sendAction === "cancel" }}
          onClick={() => {
            if (props.sendAction === "cancel") void props.onCancel();
            else void props.onSend();
          }}
          title={props.sendAction === "cancel" ? props.sendLabel : props.sendTitle}
          aria-label={props.sendLabel}
          disabled={props.sendAction === "send" && props.sendDisabled}
        >{props.sendAction === "cancel" ? "■" : "→"}</button>
      </div>
    </>
  );
}
