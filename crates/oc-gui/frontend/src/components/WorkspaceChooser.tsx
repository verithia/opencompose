/**
 * Workspace chooser — the start surface, and the dialog the File menu and the
 * AI panel's scope line reopen.
 *
 * A workspace is a directory: every `.ocp` file in it shares one set of AI
 * conversations. Picking a folder makes it the current workspace and lists its
 * project files, so the user can open one without another dialog; picking a
 * project file adopts the directory that holds it. "Continue without a
 * workspace" keeps the detached behaviour.
 *
 * The start surface is the first thing a new install shows, so it carries the
 * product mark and one primary action instead of an empty editor. A folder with
 * no project files offers to create the first one, because that is the state a
 * first run lands in.
 */
import { For, Show, createMemo } from "solid-js";
import { LOGO_MARK } from "../stores/branding";
import { i18n } from "../stores/i18n";
import { workspaceStore } from "../stores/workspace";
import { useTopmostEscape } from "./modalEscape";

/** The lists scroll inside the card, so a long folder cannot push it off screen. */
const PROJECTS_HEIGHT = "32vh";
const RECENTS_HEIGHT = "30vh";

/** A remembered folder that is gone; it stays listed so it can be forgotten. */
const MISSING_STYLE = { color: "var(--stop-red, #ef4444)", "font-size": "10px" };

export function WorkspaceChooser(props: { onClose?: () => void }) {
  const view = workspaceStore.view;
  const current = createMemo(() => view()?.path ?? null);
  const projects = createMemo(() => view()?.projects ?? []);
  const total = createMemo(() => view()?.total_projects ?? projects().length);

  return (
    <section
      class="workspace-chooser"
      classList={{ "workspace-chooser-dialog": Boolean(props.onClose) }}
      role={props.onClose ? "dialog" : "region"}
      aria-modal={props.onClose ? "true" : undefined}
      aria-label={i18n.t("workspace.title")}
    >
      <header class="workspace-hero">
        <Show when={!props.onClose}>
          <span
            class="workspace-hero-mark"
            role="img"
            aria-label="OpenCompose"
            style={{
              "mask-image": `url(${LOGO_MARK})`,
              "-webkit-mask-image": `url(${LOGO_MARK})`,
            }}
          />
        </Show>
        <div class="workspace-chooser-heading">
          <strong class="workspace-chooser-title">{i18n.t("workspace.title")}</strong>
          <span class="workspace-chooser-hint">{i18n.t("workspace.hint")}</span>
        </div>
        <Show when={props.onClose}>
          <button
            type="button"
            class="workspace-chooser-close"
            aria-label={i18n.t("workspace.close")}
            onClick={() => props.onClose?.()}
          >{"\u00D7"}</button>
        </Show>
      </header>

      <div class="workspace-body">
        <Show when={current()}>
          <div class="workspace-current">
            <span class="workspace-current-label">{i18n.t("workspace.current")}</span>
            <span class="workspace-current-name">{view()?.name ?? current()}</span>
            <Show when={view()?.exists === false}>
              <span class="workspace-missing" style={MISSING_STYLE}>{i18n.t("tooltip.notFound")}</span>
            </Show>
            <span class="workspace-current-path" title={current() ?? ""}>{current()}</span>
          </div>
        </Show>

        <Show when={current()}>
          <div class="workspace-projects">
            <div class="workspace-section-label">
              {i18n.t("workspace.projectsIn", { name: view()?.name ?? i18n.t("workspace.title") })}
              <Show when={total() > projects().length}>
                <span class="workspace-projects-count"> {i18n.t("workspace.truncated", { shown: projects().length, total: total() })}</span>
              </Show>
            </div>
            <Show
              when={projects().length}
              fallback={<div class="workspace-empty">{i18n.t("workspace.noProjects")}</div>}
            >
              <ul class="workspace-project-list" style={{ "max-height": PROJECTS_HEIGHT, "overflow-y": "auto" }}>
                <For each={projects()}>
                  {(project) => (
                    <li>
                      <button
                        type="button"
                        class="workspace-project"
                        title={project.path}
                        disabled={workspaceStore.busy()}
                        onClick={() => void workspaceStore.openProject(project.path)}
                      >
                        <span class="workspace-project-name">{project.name}</span>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
        </Show>

        <div class="workspace-recents">
          <div class="workspace-section-label">{i18n.t("workspace.recent")}</div>
          <Show
            when={workspaceStore.recents().length}
            fallback={
              <div class="workspace-empty-state">
                <span class="workspace-empty-title">{i18n.t("workspace.noRecent")}</span>
                <span class="workspace-empty-hint">{i18n.t("workspace.noRecentHint")}</span>
              </div>
            }
          >
            <ul class="workspace-recent-list" style={{ "max-height": RECENTS_HEIGHT, "overflow-y": "auto" }}>
              <For each={workspaceStore.recents()}>
                {(entry) => (
                  <li
                    class="workspace-recent"
                    classList={{ current: entry.path === current(), missing: entry.exists === false }}
                  >
                    <button
                      type="button"
                      class="workspace-recent-open"
                      disabled={workspaceStore.busy()}
                      onClick={() => void workspaceStore.open(entry.path)}
                    >
                      <span class="workspace-recent-name">{entry.name}</span>
                      <span class="workspace-recent-path" title={entry.path}>{entry.path}</span>
                      <Show when={entry.exists === false}>
                        <span class="workspace-recent-missing" style={MISSING_STYLE}>
                          {i18n.t("tooltip.notFound")}
                        </span>
                      </Show>
                    </button>
                    <button
                      type="button"
                      class="workspace-recent-forget"
                      aria-label={i18n.t("workspace.forgetOf", { name: entry.name })}
                      disabled={workspaceStore.busy()}
                      onClick={() => void workspaceStore.forget(entry.path)}
                    >{i18n.t("workspace.forget")}</button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      </div>

      <div class="workspace-actions">
        <button
          type="button"
          class="workspace-action workspace-action-open-folder"
          disabled={workspaceStore.busy()}
          onClick={() => void workspaceStore.openFolder()}
        >{i18n.t("workspace.openFolder")}</button>
        <button
          type="button"
          class="workspace-action workspace-action-open-project"
          disabled={workspaceStore.busy()}
          onClick={() => void workspaceStore.openProjectFile()}
        >{i18n.t("workspace.openProject")}</button>
        {/* Only while nothing is open: creating a project replaces the open one. */}
        <Show when={workspaceStore.startSurface() && current()}>
          <button
            type="button"
            class="workspace-action workspace-action-new-project"
            disabled={workspaceStore.busy()}
            onClick={() => void workspaceStore.newProjectHere()}
          >{i18n.t("workspace.newProject")}</button>
          <button
            type="button"
            class="workspace-action workspace-action-starter-project"
            title={i18n.t("workspace.starterHint")}
            disabled={workspaceStore.busy()}
            onClick={() => void workspaceStore.newProjectHere("starter")}
          >{i18n.t("workspace.starterProject")}</button>
        </Show>
        <button
          type="button"
          class="workspace-action workspace-action-detached"
          disabled={workspaceStore.busy()}
          onClick={() => void workspaceStore.continueDetached()}
        >{i18n.t("workspace.continueDetached")}</button>
      </div>

      <Show when={workspaceStore.error()}>
        <div class="workspace-error" role="alert">{workspaceStore.error()}</div>
      </Show>
    </section>
  );
}

/** The same chooser over the app, reached from the File menu and the panel. */
export function WorkspaceChooserDialog(props: { onClose: () => void }) {
  useTopmostEscape(() => props.onClose());
  return (
    <div class="workspace-overlay" onClick={() => props.onClose()}>
      <div class="workspace-overlay-body" onClick={(event) => event.stopPropagation()}>
        <WorkspaceChooser onClose={props.onClose} />
      </div>
    </div>
  );
}
