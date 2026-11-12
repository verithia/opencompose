/**
 * The command palette: one search field over the whole registry.
 *
 * It reads the same command table and the same keymap as the document handler,
 * so the binding shown on the right is the binding that runs. Disabled commands
 * stay in the list with the reason they cannot run.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { i18n } from "../stores/i18n";
import {
  COMMAND_CATEGORIES,
  COMMANDS,
  categoryLabel,
  commandDisabledReason,
  commandEnabled,
  commandHandlerVersion,
  commandLabel,
  runCommand,
  type Command,
} from "../stores/commands";
import { commandBindingLabel } from "../stores/keymap";
import { overlayStore } from "../stores/overlays";
import { useTopmostEscape } from "./modalEscape";
import "./CommandPalette.css";

function matches(command: Command, query: string): boolean {
  const haystack = `${commandLabel(command)} ${categoryLabel(command.category)}`.toLowerCase();
  return query.split(/\s+/).filter(Boolean).every((token) => haystack.includes(token));
}

function PaletteBody() {
  const [query, setQuery] = createSignal("");
  const [active, setActive] = createSignal(0);
  let input: HTMLInputElement | undefined;
  const previousFocus = document.activeElement as HTMLElement | null;

  useTopmostEscape(overlayStore.closePalette);

  const rows = createMemo(() => {
    // Component handlers mount and unmount; re-read them with the table.
    commandHandlerVersion();
    const value = query().trim().toLowerCase();
    return COMMANDS.filter((command) => !value || matches(command, value));
  });

  const groups = createMemo(() =>
    COMMAND_CATEGORIES
      .map((category) => ({
        category,
        items: rows().filter((command) => command.category === category),
      }))
      .filter((group) => group.items.length > 0)
  );

  createEffect(() => {
    // A new query starts at the first match, which is also what Enter runs.
    query();
    setActive(0);
  });

  const rowIndex = (command: Command) => rows().indexOf(command);

  const run = (command: Command) => {
    if (!commandEnabled(command)) return;
    overlayStore.closePalette();
    void runCommand(command.id);
  };

  const move = (delta: number) => {
    const count = rows().length;
    if (count === 0) return;
    setActive((index) => (index + delta + count) % count);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const command = rows()[active()];
      if (command) run(command);
    }
  };

  onMount(() => {
    input?.focus();
  });

  onCleanup(() => {
    previousFocus?.focus?.({ preventScroll: true });
  });

  return (
    <div
      class="command-palette-overlay"
      onPointerDown={(event) => {
        if (event.target === event.currentTarget) overlayStore.closePalette();
      }}
    >
      <div
        class="command-palette"
        role="dialog"
        aria-modal="true"
        aria-label={i18n.t("palette.title")}
        onKeyDown={onKeyDown}
      >
        <input
          ref={input}
          class="command-palette-search"
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-controls="command-palette-list"
          aria-autocomplete="list"
          aria-label={i18n.t("palette.placeholder")}
          placeholder={i18n.t("palette.placeholder")}
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
        <div class="command-palette-list" id="command-palette-list" role="listbox">
          <Show
            when={rows().length > 0}
            fallback={<p class="command-palette-empty">{i18n.t("palette.empty")}</p>}
          >
            <For each={groups()}>
              {(group) => (
                <div class="command-palette-group" role="group" aria-label={categoryLabel(group.category)}>
                  <div class="command-palette-group-title">{categoryLabel(group.category)}</div>
                  <For each={group.items}>
                    {(command) => {
                      const disabled = () => !commandEnabled(command);
                      const reason = () => commandDisabledReason(command);
                      return (
                        <button
                          type="button"
                          class="command-palette-row"
                          classList={{ active: rowIndex(command) === active(), disabled: disabled() }}
                          role="option"
                          aria-selected={rowIndex(command) === active()}
                          aria-disabled={disabled() ? "true" : undefined}
                          data-command-id={command.id}
                          onPointerEnter={() => setActive(rowIndex(command))}
                          onClick={() => run(command)}
                        >
                          <span class="command-palette-label">{commandLabel(command)}</span>
                          <Show when={disabled()}>
                            <span class="command-palette-reason">
                              {i18n.t("palette.disabled", {
                                reason: reason() ?? i18n.t("palette.disabledFallback"),
                              })}
                            </span>
                          </Show>
                          <Show when={commandBindingLabel(command.id)}>
                            {(binding) => <kbd class="command-palette-binding">{binding()}</kbd>}
                          </Show>
                        </button>
                      );
                    }}
                  </For>
                </div>
              )}
            </For>
          </Show>
        </div>
        <div class="command-palette-hint">{i18n.t("palette.hint")}</div>
      </div>
    </div>
  );
}

export function CommandPalette() {
  return (
    <Show when={overlayStore.paletteOpen()}>
      <Portal>
        <PaletteBody />
      </Portal>
    </Show>
  );
}
