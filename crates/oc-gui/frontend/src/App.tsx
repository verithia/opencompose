import { mountExternalProjectChanges } from "./stores/externalProjectChanges";
import { mountOsProjectOpen } from "./stores/osProjectOpen";
import { mountNativeMenu } from "./stores/nativeMenu";
import { ClipDetailView } from "./components/ClipDetailView";
import { onMount, onCleanup, Show, Switch, Match, For, createEffect, createSignal, createMemo, on, ErrorBoundary, type JSX } from "solid-js";
import { store } from "./stores/project";
import { prepareWebAudioHandoff } from "./stores/webAudioFocus";
import { adoptRecord, recordStore } from "./stores/record";
import { bounceStore } from "./stores/bounce";
import { automationWriteReport } from "./stores/automationWrite";

import {
  panelStore,
  BOTTOM_MODES,
  BOTTOM_KEYS,
  SIDEBAR_PANEL_IDS,
  type PanelId,
  type BottomMode,
  type SidebarPanelId,
} from "./stores/panels";
import { log } from "./stores/log";
import { i18n } from "./stores/i18n";
import { midiStore } from "./stores/midiLive";
import { Brand } from "./components/Brand";
import { WindowResizeEdges } from "./components/WindowControls";
import { BuildProgressChip } from "./components/BuildProgressChip";
import { Toolbar } from "./components/Toolbar";
import { Timeline } from "./components/Timeline";
import { Mixer } from "./components/Mixer";
import { DslEditor } from "./components/DslEditor";
import { PianoRoll } from "./components/PianoRoll";
import { PatternPanel } from "./components/PatternPanel";
import { FloatingPanel } from "./components/FloatingPanel";
import { PluginBrowser } from "./components/PluginBrowser";
import { PresetEditor } from "./components/PresetEditor";
import { DrumKitPanel } from "./components/DrumKitPanel";
import { SampleMarketPanel } from "./components/SampleMarketPanel";
import { AiPanel } from "./components/AiPanel";
import { StoreBrowser } from "./components/StoreBrowser";
import { CodeIcon, PlugIcon, SparkIcon, StoreIcon } from "./components/icons";
import { MidiImportDialog } from "./components/MidiImportDialog";
import { MidiExportDialog } from "./components/MidiExportDialog";
import { ExportAudioDialog } from "./components/ExportAudioDialog";
import { UndoHistoryPanel } from "./components/UndoHistoryPanel";
import { WorkspaceChooser, WorkspaceChooserDialog } from "./components/WorkspaceChooser";
import { LanguagePrompt } from "./components/LanguagePrompt";
import { Onboarding, Splash } from "./components/Onboarding";
import { TooltipLayer, useTooltip } from "./components/Tooltip";
import { ContextMenuLayer } from "./components/ContextMenu";
import { GainEnvelopeWindow } from "./components/GainEnvelopeWindow";
import { midiDialogStore } from "./stores/midiUi";
import { recoveryStore } from "./stores/recovery";
import { RecoveryDialog } from "./components/RecoveryDialog";
import { exportAudioStore } from "./stores/exportAudio";
import { preferencesStore } from "./stores/preferencesBundle";
import { onboardingStore } from "./stores/onboarding";
import { PreferencesDialog } from "./components/PreferencesDialog";
import { uiScaleStore } from "./stores/uiScale";
import { workspaceStore } from "./stores/workspace";
import { dispatchKeydown } from "./stores/keymap";
import { prepareQuit, projectBuildProgress, quitNow, stopRecording } from "./stores/api";
import type { BuildSnapshot } from "./types/transport";
import { QuitDialog } from "./components/QuitDialog";
import { TextPromptDialog } from "./components/TextPromptDialog";
import { ProxySuggestion } from "./components/ProxySuggestion";
import { dialogOpen } from "./components/modalEscape";
import { CommandPalette } from "./components/CommandPalette";
import { ShortcutEditor } from "./components/ShortcutEditor";
import { projectClock } from "./utils/projectClock";
import { networkStore } from "./stores/network";
import { beginPointerDrag } from "./utils/pointerDrag";
import type { MidiImportResult } from "./stores/midi";

export default function App() {
  // Asked once per machine; the stored choice is what later launches read.
  const [languageChosen, setLanguageChosen] = createSignal(i18n.hasStoredLocale());
  const [splashDone, setSplashDone] = createSignal(!onboardingStore.pending());
  const [quitPrompt, setQuitPrompt] = createSignal(false);
  const [quitRecording, setQuitRecording] = createSignal(false);
  const [quitBusy, setQuitBusy] = createSignal(false);
  // Where the request came from: a window close leaves the window, a quit
  // leaves the process.
  const [historyOpen, setHistoryOpen] = createSignal(false);
  /** What the render thread is preparing, polled independently of playback. */
  const [buildProgress, setBuildProgress] = createSignal<BuildSnapshot | null>(null);
  /** The project name the operator is editing; the webview has no prompt. */
  const [renameProject, setRenameProject] = createSignal<string | null>(null);
  let buildPollFastUntil = 0;
  let dividerRef: HTMLDivElement | undefined;
  let splitContainerRef: HTMLDivElement | undefined;

  const statusSelection = createMemo(() => {
    const track = store.selectedTrack();
    const pattern = store.selectedPattern();
    if (pattern && track) return `${track.name} / ${pattern.name}`;
    if (pattern) return pattern.name;
    if (track) return track.name;
    return i18n.t("status.noSelection");
  });

  const statusKey = createMemo(() => {
    const key = store.project.properties.key;
    if (!key) return i18n.t("status.keyUnset");
    const accidental = key.accidental === "Sharp" ? "#" : key.accidental === "Flat" ? "b" : "";
    return i18n.t("status.key", { key: `${key.root}${accidental} ${key.mode}` });
  });

  const displaySeconds = createMemo(() => {
    const scrub = store.scrubPosition();
    if (scrub !== null) return projectClock(store.project.properties).secondsAt(scrub);
    const privateSeconds = store.auditionPositionSeconds();
    if (privateSeconds !== null) return privateSeconds;
    const displayedBeat = store.previewCursorBeats();
    if (displayedBeat === null) return store.transport().position_seconds;
    return projectClock(store.project.properties).secondsAt(displayedBeat);
  });

  const transportLabel = createMemo(() => {
    const t = store.transport();
    if (store.playbackPlaying()) return i18n.t("transport.play");
    if (store.auditionActive()) return i18n.t("transport.paused");
    if (t.position_seconds > 0) return i18n.t("transport.paused");
    return i18n.t("transport.stop");
  });

  const panelLabel = (id: PanelId) => i18n.t(`panel.${id}` as Parameters<typeof i18n.t>[0]);

  // Keep the Space target visible: the status bar names what the next press hits.
  const spaceTarget = createMemo(() => {
    if (store.auditionActive()) return i18n.t("status.spaceAudition");
    const command = panelStore.spaceCommand(
      panelStore.activeRegion(),
      panelStore.bottomMode(),
      store.selectedPresetName(),
      store.selectedPatternName(),
      store.auditionActive(),
    );
    switch (command.kind) {
      case "audition-preset":
        return i18n.t("status.spacePreset", { preset: command.preset });
      case "audition-pattern":
        return i18n.t("status.spaceClip", { clip: command.pattern });
      case "toggle-audition":
        return i18n.t("status.spaceAudition");
      default:
        return i18n.t("status.spaceTimeline");
    }
  });

  onMount(() => {
    // A structural edit rebuilds every instrument while stopped, so the chip
    // needs its own poll: the transport poll only runs during playback.
    let buildPollStopped = false;
    let buildPollTimer: number | undefined;
    const pollBuild = async () => {
      if (buildPollStopped) return;
      let next: BuildSnapshot | null = null;
      try {
        const snapshot = await projectBuildProgress();
        // An older backend or a test double answers with another shape.
        next = snapshot && typeof snapshot.stage === "string" ? snapshot : null;
      } catch {
        next = null;
      }
      if (buildPollStopped) return;
      setBuildProgress(next);
      const fast = next !== null || performance.now() < buildPollFastUntil;
      buildPollTimer = window.setTimeout(pollBuild, fast ? 120 : 1200);
    };
    buildPollTimer = window.setTimeout(pollBuild, 300);
    onCleanup(() => {
      buildPollStopped = true;
      if (buildPollTimer !== undefined) window.clearTimeout(buildPollTimer);
    });
    // Any document revision may start a build; ask quickly afterwards.
    createEffect(on(() => store.projectRevision, () => {
      buildPollFastUntil = performance.now() + 8000;
    }));
    onCleanup(mountNativeMenu({
      blocked: () => dialogOpen() || onboardingStore.pending() || !languageChosen() || quitPrompt(),
      onQuit: () => void askToQuit(),
    }));
    if (!splashDone()) {
      const timer = window.setTimeout(() => setSplashDone(true), 1400);
      onCleanup(() => window.clearTimeout(timer));
    }
    // The workspace is read after the startup project: a file passed on the
    // command line already scopes the conversations to its directory.
    const startupReady = store.initStartupProject().then(() => workspaceStore.init());
    onCleanup(mountExternalProjectChanges(startupReady));
    onCleanup(mountOsProjectOpen(startupReady,
      () => dialogOpen() || onboardingStore.pending() || !languageChosen() || quitPrompt()));
    uiScaleStore.init();
    void midiStore.init();
    splitContainerRef?.addEventListener("pointerdown", handleRegionPointerDown, true);
    splitContainerRef?.addEventListener("focusin", handleRegionPointerDown, true);
    log.info("app", "OpenCompose DAW started");
    void networkStore.detect();

    // Close ends the document window (and the app when it is the last one).
    // The OS must not destroy it before the asynchronous Save/Discard decision.
    let disposed = false;
    const listeners: (() => void)[] = [];
    const ownListener = (release: () => void) => {
      if (disposed) release();
      else listeners.push(release);
    };
    onCleanup(() => { disposed = true; listeners.forEach(release => release()); });
    void prepareWebAudioHandoff().catch((error) => log.warn("audio", String(error)));
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      const win = getCurrentWindow();
      return win.onCloseRequested(async (event) => {
        event.preventDefault();
        await askToQuit();
      });
    }).then(ownListener).catch(error => log.warn("app", String(error)));
    // Quit from the menu/platform uses the same guarded document decision.
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen("quit_requested", () => void askToQuit()),
    ).then(ownListener).catch(error => log.warn("app", String(error)));
  });

  // ── Quit handlers ──
  let preparingQuit = false;
  async function askToQuit() {
    if (preparingQuit || quitPrompt()) return;
    preparingQuit = true;
    try {
      const result = await prepareQuit();
      setQuitRecording(result.recording);
      if (result.recording || result.dirty || store.dirty()) setQuitPrompt(true);
      else await leave();
    } catch (error) {
      store.setError(String(error));
    } finally {
      preparingQuit = false;
    }
  }

  async function finishQuitTake(): Promise<boolean> {
    if (!quitRecording()) return true;
    try {
      const result = await stopRecording();
      if (result.project) store.applyTakeSnapshot(result.project);
      adoptRecord(result.record);
      store.adoptTransport(result);
      setQuitRecording(false);
      return true;
    } catch (error) {
      store.setError(String(error));
      return false;
    }
  }

  async function confirmQuit(saveChanges: boolean) {
    if (quitBusy()) return;
    setQuitBusy(true);
    try {
      if (!await finishQuitTake()) return;
      if (saveChanges) {
        if (store.filePath()) {
          if (!await store.saveFile()) return;
        } else {
          const { save } = await import("@tauri-apps/plugin-dialog");
          const path = await save({ filters: [{ name: "OpenCompose", extensions: ["ocp"] }] });
          if (!path || !await store.saveFile(path as string)) return;
        }
      } else if (!await store.discardUnsaved()) return;
      await leave();
    } catch (error) {
      store.setError(String(error));
    } finally {
      setQuitBusy(false);
    }
  }

  const handleQuitCancel = () => {
    // The close event was prevented when the prompt opened and the document is
    // still dirty, so hiding the prompt is the whole answer.
    if (!quitBusy()) setQuitPrompt(false);
  };

  /** There is one document window. After its decision, exit the process so
   * a window destroy cannot leave an invisible single-instance owner behind. */
  async function leave() {
    try {
      await quitNow();
      setQuitPrompt(false);
    } catch (error) {
      store.setError(String(error));
    }
  }

  const handleMidiImported = (result: MidiImportResult, firstNewTrack: number | null) => {
    store.applyMidiImportSnapshot(result.snapshot, firstNewTrack);
  };


  // ── Divider drag ──
  // Moves are coalesced into one ratio update per frame: the panel height is the
  // live pointer position, so writing it per event buys nothing and re-lays-out
  // the docked panel for every pointer sample.
  const handleDividerPointerDown = (e: PointerEvent & { currentTarget: HTMLDivElement }) => {
    if (e.button !== 0 || !splitContainerRef) return;
    e.preventDefault();

    const containerRect = splitContainerRef.getBoundingClientRect();
    const containerHeight = containerRect.height;
    let pendingY: number | null = null;
    let frame = 0;

    const flush = () => {
      frame = 0;
      if (pendingY === null || containerHeight <= 0) return;
      const y = pendingY - containerRect.top;
      pendingY = null;
      panelStore.updateSplitRatio(y / containerHeight);
    };
    beginPointerDrag({
      element: e.currentTarget,
      pointerId: e.pointerId,
      cursor: "row-resize",
      onMove: (_x, clientY) => {
        pendingY = clientY;
        if (frame === 0) frame = requestAnimationFrame(flush);
      },
      onEnd: () => {
        if (frame !== 0) cancelAnimationFrame(frame);
        flush();
        panelStore.endDividerDrag();
      },
    });
    panelStore.startDividerDrag();
  };

  // ── Sidebar drag ──
  // The width is measured on the sidebar, not read from the store: the store
  // holds the requested width, while the CSS may have clamped the rendered one.
  const handleSidebarPointerDown = (e: PointerEvent & { currentTarget: HTMLDivElement }) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const sidebar = e.currentTarget.nextElementSibling;
    const startWidth = sidebar?.getBoundingClientRect().width || panelStore.sidebarWidth();
    beginPointerDrag({
      element: e.currentTarget,
      pointerId: e.pointerId,
      cursor: "col-resize",
      onMove: (clientX) => {
        const delta = startX - clientX;
        panelStore.setSidebarWidth(Math.max(220, Math.min(600, startWidth + delta)));
      },
    });
  };

  // ── Active editing region (capture: clip handlers may stop bubbling) ──
  const handleRegionPointerDown = (event: Event) => {
    const target = event.target as HTMLElement | null;
    if (target?.closest(".split-bottom")) panelStore.setActiveRegion("bottom");
    else if (target?.closest(".split-top")) panelStore.setActiveRegion("timeline");
  };

  // ── Global keyboard shortcuts ──
  // Every shortcut is a command; Escape stays here because it belongs to the
  // surface that is open (a take, a dialog, the bottom panel), not to a command.
  const handleKeyDown = (e: KeyboardEvent) => {
    const isMod = e.metaKey || e.ctrlKey;
    const active = document.activeElement as HTMLElement | null;
    const tag = active?.tagName?.toLowerCase();
    const inInput = tag === "input" || tag === "textarea" || tag === "select";
    if (!isMod && !inInput && e.key === "Escape") {
      // An open dialog owns the key: its own handler cancels it, and the take
      // and the bottom panel stay untouched behind it.
      if (e.defaultPrevented || dialogOpen()) return;
      // Escape = stop the take before anything else.
      if (recordStore.recording()) {
        e.preventDefault();
        void recordStore.stop();
        return;
      }
      if (panelStore.bottomPanel() !== null) {
        panelStore.closeBottom();
        return;
      }
    }
    dispatchKeydown(e);
  };

  onMount(() => document.addEventListener("keydown", handleKeyDown));
  onCleanup(() => document.removeEventListener("keydown", handleKeyDown));
  // A smaller window can leave a floating panel's title bar outside it.
  onMount(() => window.addEventListener("resize", panelStore.reclampFloatingPanels));
  onCleanup(() => window.removeEventListener("resize", panelStore.reclampFloatingPanels));
  onCleanup(() => {
    splitContainerRef?.removeEventListener("pointerdown", handleRegionPointerDown, true);
    splitContainerRef?.removeEventListener("focusin", handleRegionPointerDown, true);
  });

  // ── Split ratio CSS ──
  const topHeight = () => {
    if (panelStore.bottomPanel() === null) return "100%";
    return `${panelStore.splitRatio() * 100}%`;
  };
  const bottomHeight = () => {
    if (panelStore.bottomPanel() === null) return "0%";
    return `${(1 - panelStore.splitRatio()) * 100}%`;
  };

  return (
    <ErrorBoundary fallback={(err, reset) => (
      <div class="daw-root" style={{ display: "flex", "flex-direction": "column", "align-items": "center", "justify-content": "center", height: "100vh", gap: "12px", padding: "24px" }}>
        <div style={{ "font-size": "18px", "font-weight": "600", color: "var(--stop-red, #ef4444)" }}>{i18n.t("dialog.errorTitle")}</div>
        <pre class="selectable" style={{ "font-size": "12px", color: "var(--text-muted, #888)", "max-width": "600px", overflow: "auto", "white-space": "pre-wrap" }}>{String(err)}</pre>
        <button onClick={reset} style={{ padding: "8px 16px", background: "var(--accent, #7c3aed)", color: "white", border: "none", "border-radius": "6px", cursor: "pointer" }}>{i18n.t("dialog.retry")}</button>
      </div>
    )}>
    <div class="daw-root">
      <Brand />
      <ProxySuggestion />
      <WindowResizeEdges />

      <Toolbar />

      <div class="daw-body">
        {/* ── Main content: split top/bottom ── */}
        <div
          class="daw-main"
          classList={{ resizing: panelStore.isDividerDragging() }}
          ref={splitContainerRef}
        >
          {/* Top: the start surface until a workspace is open, then the timeline */}
          <div
            class="split-top"
            classList={{ "region-active": panelStore.activeRegion() === "timeline" }}
            style={{ height: topHeight(), "min-height": "120px" }}
          >
            <Show
              when={!workspaceStore.startSurface()}
              fallback={<div class="workspace-start"><WorkspaceChooser /></div>}
            >
              <Timeline />
            </Show>
          </div>

          {/* Divider + Bottom (visible only when bottom is open) */}
          <Show when={panelStore.bottomPanel() !== null}>
            {/* Thin drag divider */}
            <div
              ref={dividerRef}
              class="split-divider"
              classList={{ dragging: panelStore.isDividerDragging() }}
              onPointerDown={handleDividerPointerDown}
            />

            {/* Bottom panel */}
            <div
              class="split-bottom"
              classList={{ "region-active": panelStore.activeRegion() === "bottom" }}
              style={{ height: bottomHeight(), "min-height": "100px" }}
            >
              {/* Tab strip header */}
              <div class="bottom-panel-tabs">
                <For each={BOTTOM_MODES}>
                  {(mode) => {
                    // Don't show "detail" in the tab strip — it's opened by interaction only
                    if (mode === "detail") return null;
                    const key = BOTTOM_KEYS[mode];
                    return (
                      <button
                        class="bottom-tab"
                        classList={{ active: panelStore.bottomMode() === mode }}
                        onClick={() => panelStore.openBottom(mode)}
                        {...useTooltip(() => `${panelLabel(mode)}${key ? ` (${key})` : ""}`)}
                      >
                        {panelLabel(mode)}
                        {key && <span class="bottom-tab-key">{key}</span>}
                      </button>
                    );
                  }}
                </For>
                {/* Show "Detail" tab only when in detail mode */}
                <Show when={panelStore.bottomMode() === "detail"}>
                  <button class="bottom-tab active">
                    {panelLabel("detail")}
                  </button>
                </Show>
                <div style={{ flex: "1" }} />
                <button
                  class="bottom-tab bottom-tab-close"
                  onClick={() => panelStore.closeBottom()}
                  {...useTooltip(() => i18n.t("panel.close"))}
                >
                  &#x2715;
                </button>
              </div>

              {/* Panel content */}
              <div style={{ flex: "1", overflow: "hidden" }}>
                <BottomPanelContent mode={panelStore.bottomMode()!} />
              </div>
            </div>
          </Show>

          {/* Status bar */}
          <div class="status-bar">
            <div class="status-left">
              <Show when={store.error()}>
                <span class="status-error selectable">{store.error()}</span>
              </Show>
              <Show when={!store.error()}>
                <span class="status-chip strong">{transportLabel()}</span>
                <span class="status-chip">{store.project.tracks.length} {i18n.t("status.tracks")}</span>
                <span class="status-chip">{store.project.buses.length} {i18n.t("status.buses")}</span>
                <span class="status-chip">{statusKey()}</span>
                <span class="status-chip selected">{i18n.t("status.selection", { selection: statusSelection() })}</span>
                <span class="status-chip space-target" data-space-target={spaceTarget()}>
                  {spaceTarget()}
                </span>
                <Show when={bounceStore.pending()}>
                  <span class="status-chip loading">{i18n.t("bounce.pendingBounce")}</span>
                </Show>
                <Show when={bounceStore.report()}>
                  {(line) => (
                    <span class="status-chip bounce-report" data-bounce-report={line()}>
                      {line()}
                    </span>
                  )}
                </Show>
                <Show when={automationWriteReport()}>
                  {(line) => (
                    <span class="status-chip automation-write-report" data-automation-write-report={line()}>
                      {line()}
                    </span>
                  )}
                </Show>
                <Show when={!store.filePath()}>
                  <span class="status-chip">{i18n.t("status.unsaved")}</span>
                </Show>
              </Show>
            </div>
            <div
              class="status-project"
              title={i18n.t("status.renameProjectHint")}
              onDblClick={() => setRenameProject(store.project.name ?? "")}
            >
              {store.project.name ?? i18n.t("status.untitled")}
              <Show when={store.dirty()}>
                <span class="dirty-indicator">*</span>
              </Show>
            </div>
            <div class="status-right">
              <button
                class="status-chip undo-history-toggle"
                aria-label={i18n.t("history.title")}
                aria-expanded={historyOpen()}
                onClick={() => setHistoryOpen((open) => !open)}
              >{i18n.t("history.title")}</button>
              <span class="status-transport-time">
                {formatTime(displaySeconds())}
                {" / "}
                {formatTime(store.transport().duration_seconds)}
              </span>
              <Show when={store.loading()}>
                <span class="status-chip loading">{i18n.t("status.rendering")}</span>
              </Show>
              <BuildProgressChip build={buildProgress() ?? store.transport().build} />
            </div>
          </div>
        </div>

        {/* ── Right sidebar ── */}
        <Show when={panelStore.sidebarOpen()}>
          {/* Resize handle (left edge of sidebar) */}
          <div
            class="sidebar-resize-handle"
            onPointerDown={handleSidebarPointerDown}
          />
          <div
            class="right-sidebar"
            style={{ width: `${panelStore.sidebarWidth()}px` }}
          >
            {/* One row above the panel: the tabs, the open panel's name, then
                the collapse control. The tabs were a full-height column, so the
                panel below now owns that width. */}
            <div class="sidebar-topbar">
              <div class="sidebar-tabs">
                <For each={SIDEBAR_PANEL_IDS}>
                  {(id) => (
                    <button
                      class="sidebar-tab"
                      classList={{ active: panelStore.sidebarTab() === id }}
                      aria-label={panelLabel(id)}
                      onClick={() => panelStore.selectSidebarTab(id)}
                      {...useTooltip(() => panelLabel(id))}
                    >
                      {sidebarIcon(id)}
                    </button>
                  )}
                </For>
              </div>
              <span class="sidebar-tab-name">{panelLabel(panelStore.sidebarTab())}</span>
              <button
                class="sidebar-collapse"
                aria-label={i18n.t("panel.collapseSidebar")}
                onClick={() => panelStore.closeSidebar()}
                {...useTooltip(() => i18n.t("panel.collapseSidebar"))}
              >&#x276F;</button>
            </div>
            {/* Sidebar content */}
            <div class="sidebar-content">
              <SidebarPanelContent id={panelStore.sidebarTab()} />
            </div>
          </div>
        </Show>

        {/* Sidebar toggle (vertical strip when closed) */}
        <Show when={!panelStore.sidebarOpen()}>
          <div
            class="sidebar-toggle-strip"
            onClick={() => panelStore.openSidebar()}
            {...useTooltip(() => i18n.t("panel.openSidebar"))}
          >
            <span class="sidebar-toggle-icon">&#x276F;</span>
          </div>
        </Show>

        {/* ── Floating panels ── */}
        <For each={panelStore.floatingPanels}>
          {(fp) => (
            <FloatingPanel panel={{
              id: fp.id, mode: "floating",
              x: fp.x, y: fp.y, width: fp.width, height: fp.height,
              minimized: fp.minimized, zIndex: fp.zIndex,
            }}>
              <AnyPanelContent id={fp.id} />
            </FloatingPanel>
          )}
        </For>
      </div>

      <UndoHistoryPanel open={historyOpen()} onClose={() => setHistoryOpen(false)} />

      {/* The chooser, reopened from the File menu or the AI panel's scope line */}
      <Show when={!languageChosen()}>
        <LanguagePrompt onDone={() => setLanguageChosen(true)} />
      </Show>

      <Show when={splashDone() && languageChosen() && onboardingStore.pending()}>
        <Onboarding onDone={() => onboardingStore.complete()} />
      </Show>

      <Show when={!splashDone()}>
        <Splash />
      </Show>

      <Show when={workspaceStore.chooserOpen()}>
        <WorkspaceChooserDialog onClose={() => workspaceStore.closeChooser()} />
      </Show>

      {/* MIDI import / export dialogs */}
      <Show when={midiDialogStore.importRequest()}>
        {(request) => (
          <MidiImportDialog
            path={request().path}
            preferredTrack={request().preferredTrack}
            onClose={() => midiDialogStore.closeImport()}
            onImported={handleMidiImported}
          />
        )}
      </Show>
      <Show when={midiDialogStore.exportRequest()}>
        {(request) => (
          <MidiExportDialog
            trackIndex={request().trackIndex}
            onClose={() => midiDialogStore.closeExport()}
          />
        )}
      </Show>

      {/* Recovery: the startup prompt and the File menu's dialog */}
      <Show when={recoveryStore.promptOpen()}>
        <RecoveryDialog prompt onClose={() => recoveryStore.close()} />
      </Show>
      <Show when={recoveryStore.open()}>
        <RecoveryDialog prompt={false} onClose={() => recoveryStore.close()} />
      </Show>
      {/* The one audio export dialog: options, progress, finished summary */}
      <Show when={exportAudioStore.dialogRequest()}>
        {(request) => (
          <ExportAudioDialog
            scope={request().scope}
            onClose={() => exportAudioStore.close()}
          />
        )}
      </Show>

      {/* Preferences bundle: export, or a confirmed import with its report */}
      <Show when={preferencesStore.request()}>
        {(request) => (
          <PreferencesDialog request={request()} onClose={() => preferencesStore.close()} />
        )}
      </Show>

      {/* Quit confirmation modal */}
      <Show when={renameProject()}>
        {(name) => (
          <TextPromptDialog
            title={i18n.t("status.renameProject")}
            initial={name()}
            onCancel={() => setRenameProject(null)}
            onConfirm={(next) => {
              setRenameProject(null);
              void store.setProjectName(next);
            }}
          />
        )}
      </Show>

      <Show when={quitPrompt()}>
        <QuitDialog
          body={quitRecording() ? i18n.t("dialog.recordingQuitBody") : undefined}
          busy={quitBusy()}
          onSave={() => void confirmQuit(true)}
          onDiscard={() => void confirmQuit(false)}
          onCancel={handleQuitCancel}
        />
      </Show>
      <TooltipLayer />
      <GainEnvelopeWindow />
      <ContextMenuLayer />
      <CommandPalette />
      <ShortcutEditor />
    </div>
    </ErrorBoundary>
  );
}

// ── Bottom panel router ──
function BottomPanelContent(props: { mode: BottomMode }) {
  return (
    <Switch>
      <Match when={props.mode === "clips"}><PatternPanel /></Match>
      <Match when={props.mode === "pianoroll"}><PianoRoll /></Match>
      <Match when={props.mode === "presets"}><PresetEditor /></Match>
      <Match when={props.mode === "mixer"}><Mixer /></Match>
      <Match when={props.mode === "drumpads"}><DrumKitPanel /></Match>
      <Match when={props.mode === "samples"}><SampleMarketPanel /></Match>
      <Match when={props.mode === "detail"}><DetailView /></Match>
    </Switch>
  );
}

// ── Detail view: split left (pattern attrs + preset) / right (piano roll) ──
function DetailView() { return <ClipDetailView />; }

// ── Sidebar panel router ──
function SidebarPanelContent(props: { id: SidebarPanelId }) {
  return (
    <Switch>
      <Match when={props.id === "dsl"}><DslEditor /></Match>
      <Match when={props.id === "plugins"}><PluginBrowser /></Match>
      <Match when={props.id === "store"}><StoreBrowser /></Match>
      <Match when={props.id === "ai"}><AiPanel onClose={() => panelStore.closePanel(props.id)} /></Match>
    </Switch>
  );
}

// ── Any panel (for floating) ──
function AnyPanelContent(props: { id: PanelId }) {
  return (
    <Switch>
      <Match when={props.id === "clips"}><PatternPanel /></Match>
      <Match when={props.id === "pianoroll"}><PianoRoll /></Match>
      <Match when={props.id === "presets"}><PresetEditor /></Match>
      <Match when={props.id === "mixer"}><Mixer /></Match>
      <Match when={props.id === "drumpads"}><DrumKitPanel /></Match>
      <Match when={props.id === "samples"}><SampleMarketPanel /></Match>
      <Match when={props.id === "detail"}><DetailView /></Match>
      <Match when={props.id === "dsl"}><DslEditor /></Match>
      <Match when={props.id === "plugins"}><PluginBrowser /></Match>
      <Match when={props.id === "store"}><StoreBrowser /></Match>
      <Match when={props.id === "ai"}><AiPanel onClose={() => panelStore.closePanel(props.id)} /></Match>
    </Switch>
  );
}

// ── Sidebar icons (compact glyphs for the narrow tab strip) ──
function sidebarIcon(id: SidebarPanelId): JSX.Element {
  switch (id) {
    case "dsl": return <CodeIcon />;
    case "plugins": return <PlugIcon />;
    case "store": return <StoreIcon />;
    case "ai": return <SparkIcon />;
  }
}

function formatTime(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  const ms = Math.floor((seconds % 1) * 10);
  return `${m}:${s.toString().padStart(2, "0")}.${ms}`;
}
