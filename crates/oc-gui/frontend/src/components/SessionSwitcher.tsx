import { For, Show, createEffect, createUniqueId, onCleanup } from "solid-js";
import type { PermissionMode } from "../ai/permission";
import { updatedLabel, type SessionMeta } from "../ai/sessions";
import { i18n } from "../stores/i18n";
import { AiPermissionSelect } from "./AiPermissionSelect";
import { useTooltip } from "./Tooltip";
import { dialogOpen } from "./modalEscape";

export interface SessionMenu {
  x: number;
  y: number;
  session: SessionMeta;
}

interface RenameDraft {
  id: string;
  title: string;
}

export function SessionSwitcher(props: {
  title: string;
  activeSessionId: string | null;
  sessions: readonly SessionMeta[];
  workspacePath: string | null;
  open: boolean;
  busy: boolean;
  exportOpen: boolean;
  exportDisabled: boolean;
  permission: PermissionMode;
  renaming: RenameDraft | null;
  deleting: string | null;
  menu: SessionMenu | null;
  error: string | null;
  onToggle: () => void;
  onNew: () => void;
  onToggleExport: () => void;
  onPermissionChange: (mode: PermissionMode) => void;
  onChooseWorkspace: () => void;
  onSwitch: (id: string) => Promise<void>;
  onShowMenu: (menu: SessionMenu | null) => void;
  onRename: (session: SessionMeta) => void;
  onRenameDraft: (draft: RenameDraft | null) => void;
  onCommitRename: () => Promise<void>;
  onConfirmDelete: (id: string | null) => void;
  onDelete: (id: string) => Promise<void>;
  onFork: (id: string) => Promise<void>;
  onCompact: (id: string) => Promise<void>;
  onOriginal: (id: string) => Promise<void>;
}) {
  let trigger: HTMLButtonElement | undefined;
  const listId = createUniqueId();
  createEffect(() => {
    if (!props.open) return;
    // AX activation need not move DOM focus to the trigger or its list. Nested
    // controls/dialogs consume Escape first; otherwise the open picker owns it.
    const dismiss = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.isComposing || event.defaultPrevented || dialogOpen()) return;
      event.preventDefault();
      event.stopPropagation();
      if (props.menu) props.onShowMenu(null);
      else if (props.renaming) props.onRenameDraft(null);
      else if (props.deleting) props.onConfirmDelete(null);
      else props.onToggle();
      trigger?.focus({ preventScroll: true });
    };
    window.addEventListener("keydown", dismiss);
    onCleanup(() => window.removeEventListener("keydown", dismiss));
  });
  return (
    <div class="ai-session-bar">
      <button
        type="button"
        class="ai-session-current"
        aria-label={i18n.t("ai.sessions.toggle")}
        aria-expanded={props.open}
        aria-controls={props.open ? listId : undefined}
        ref={trigger}
        disabled={props.busy}
        onClick={props.onToggle}
      >
        <span class="ai-session-current-title">{props.title}</span>
        <span class="ai-session-caret" aria-hidden="true">{"▾"}</span>
      </button>
      <button
        type="button"
        class="ai-session-new"
        aria-label={i18n.t("ai.sessions.new")}
        onClick={props.onNew}
        {...useTooltip(() => i18n.t("ai.sessions.new"))}
      >{"+"}</button>
      <button
        type="button"
        class="ai-session-export"
        aria-label={i18n.t("ai.export.action")}
        aria-expanded={props.exportOpen}
        disabled={props.exportDisabled}
        onClick={props.onToggleExport}
        {...useTooltip(() => i18n.t("ai.export.action"))}
      >{i18n.t("ai.export.short")}</button>
      {/* What this conversation may do to the DSL without asking. It is the
          same control the settings dialog shows, and it names the current
          mode in the bar. */}
      <AiPermissionSelect
        value={props.permission}
        onChange={props.onPermissionChange}
        disabled={props.busy}
      />
      <Show when={props.open}>
        <div id={listId} class="ai-session-list" role="list" aria-label={i18n.t("ai.sessions.toggle")}>
          {/* The scope, once: conversations belong to this directory, not to one
              file. The line switches the workspace, so it is the second way back
              to the chooser. */}
          <button
            type="button"
            class="ai-session-scope"
            title={props.workspacePath ?? i18n.t("ai.sessions.detached")}
            aria-label={i18n.t("workspace.switch")}
            onClick={props.onChooseWorkspace}
          >
            <span class="ai-session-scope-label">{i18n.t("ai.sessions.workspace")}</span>
            <span class="ai-session-scope-path">{props.workspacePath ?? i18n.t("ai.sessions.detached")}</span>
          </button>
          <Show when={props.sessions.length} fallback={<div class="ai-session-empty">{i18n.t("ai.sessions.empty")}</div>}>
            <For each={props.sessions}>
              {(session) => (
                <div
                  class="ai-session-row"
                  role="listitem"
                  data-session-row={session.id}
                  tabindex="-1"
                  classList={{ open: session.id === props.activeSessionId }}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    props.onShowMenu({ x: event.clientX, y: event.clientY, session });
                  }}
                >
                  <Show when={props.renaming?.id === session.id} fallback={
                    <>
                      <button type="button" class="ai-session-row-main" onClick={() => void props.onSwitch(session.id)}>
                        <span class="ai-session-row-title">{session.title}</span>
                        <Show when={updatedLabel(session.updated)}>
                          <time class="ai-session-row-time" datetime={session.updated} title={session.updated}>
                            {updatedLabel(session.updated)}
                          </time>
                        </Show>
                      </button>
                      <Show when={props.deleting === session.id} fallback={
                        <span class="ai-session-row-hint">{i18n.t("ai.sessions.rowHint")}</span>
                      }>
                        <div class="ai-session-confirm">
                          <span>{i18n.t("ai.sessions.deleteConfirm", { title: session.title })}</span>
                          <button
                            type="button"
                            class="ai-session-action ai-session-action-delete"
                            disabled={props.busy}
                            onClick={() => void props.onDelete(session.id)}
                          >{i18n.t("ai.sessions.delete")}</button>
                          <button
                            type="button"
                            class="ai-session-action"
                            onClick={() => props.onConfirmDelete(null)}
                          >{i18n.t("ai.sessions.cancel")}</button>
                        </div>
                      </Show>
                    </>
                  }>
                    <input
                      class="ai-session-rename"
                      aria-label={i18n.t("ai.sessions.renameLabel")}
                      value={props.renaming!.title}
                      onInput={(e) => props.onRenameDraft({ id: session.id, title: e.currentTarget.value })}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); void props.onCommitRename(); }
                        else if (e.key === "Escape" && !e.isComposing) {
                          e.preventDefault();
                          e.stopPropagation();
                          props.onRenameDraft(null);
                          trigger?.focus({ preventScroll: true });
                        }
                      }}
                      onBlur={() => void props.onCommitRename()}
                      ref={(element) => queueMicrotask(() => element.focus())}
                    />
                  </Show>
                </div>
              )}
            </For>
          </Show>
          <Show when={props.error}><div class="ai-session-error selectable">{props.error}</div></Show>
        </div>

      <Show when={props.menu}>
          {(menu) => (
            <div
              class="ai-session-menu"
              role="menu"
              style={{
                left: `${Math.min(menu().x, window.innerWidth - 180)}px`,
                top: `${Math.min(menu().y, window.innerHeight - 150)}px`,
              }}
              onKeyDown={(event) => {
                const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')];
                const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
                const close = () => {
                  event.preventDefault();
                  event.stopPropagation();
                  const rowId = menu().session.id;
                  props.onShowMenu(null);
                  // Focus returns to the row the menu described, after the
                  // re-render that closes the menu.
                  setTimeout(() => {
                    document
                      .querySelector<HTMLElement>(`.ai-session-row[data-session-row="${rowId}"]`)
                      ?.focus({ preventScroll: true });
                  }, 0);
                };
                if (event.key === "Escape") return close();
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  event.stopPropagation();
                  const step = event.key === "ArrowDown" ? 1 : -1;
                  const next = buttons[(current + step + buttons.length) % buttons.length];
                  next?.focus({ preventScroll: true });
                } else if (event.key === "Home" || event.key === "End") {
                  event.preventDefault();
                  buttons[event.key === "Home" ? 0 : buttons.length - 1]?.focus({ preventScroll: true });
                } else if (event.key === "Tab") {
                  props.onShowMenu(null);
                }
              }}
            >
              <button
                type="button"
                role="menuitem"
                disabled={props.busy}
                onClick={() => { props.onRename(menu().session); props.onShowMenu(null); }}
              >{i18n.t("ai.sessions.rename")}</button>
              <button
                type="button"
                role="menuitem"
                disabled={props.busy}
                onClick={() => { void props.onFork(menu().session.id); props.onShowMenu(null); }}
              >{i18n.t("ai.sessions.fork")}</button>
              <button
                type="button"
                role="menuitem"
                disabled={props.busy}
                onClick={() => { void props.onCompact(menu().session.id); props.onShowMenu(null); }}
              >{i18n.t("ai.sessions.compact")}</button>
            <button
              type="button"
              role="menuitem"
              onClick={() => { void props.onOriginal(menu().session.id); props.onShowMenu(null); }}
            >{i18n.t("ai.sessions.original")}</button>
              <button
                type="button"
                role="menuitem"
                class="ai-session-menu-delete"
                disabled={props.busy}
                onClick={() => { props.onConfirmDelete(menu().session.id); props.onShowMenu(null); }}
              >{i18n.t("ai.sessions.delete")}</button>
            </div>
          )}
        </Show>
      </Show>

    </div>
  );
}
