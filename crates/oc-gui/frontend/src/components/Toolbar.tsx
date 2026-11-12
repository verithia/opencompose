/**
 * Main application toolbar — transport, file ops, tempo, panels.
 *
 * The left-hand controls live in a strip that shrinks and wraps; Preferences
 * and Browse sit outside it, so a large interface scale cannot push them off
 * screen. Labels hide before controls do.
 */
import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { store } from "../stores/project";
import { panelStore } from "../stores/panels";
import { recoveryStore } from "../stores/recovery";
import { workspaceStore } from "../stores/workspace";
import { i18n } from "../stores/i18n";
import { projectClock } from "../utils/projectClock";
import { openMidiExportDialog, openMidiImportDialog } from "../stores/midiUi";
import { openExportAudioDialog } from "../stores/exportAudio";
import { clickStore } from "../stores/click";
import { recordStore } from "../stores/record";
import { automationWriteArmed, toggleAutomationWriteArm } from "../stores/automationWrite";
import { commandBindingLabel } from "../stores/keymap";
import { EXPORT_DIAGNOSTICS_EVENT } from "../stores/appEvents";
import { newProjectDialog, openAlignedStemsDialog, openFileDialog, saveAsFileDialog, saveFileDialog } from "../stores/fileDialogs";
import { diagnosticsStore } from "../stores/diagnosticsUi";
import { useTooltip } from "./Tooltip";
import { WindowControls } from "./WindowControls";
import { PreferencesHost } from "./PreferencesModal";
import { preferencesUi } from "../stores/preferencesUi";
import { ResourceBrowser } from "./ResourceBrowser";
import { QuitDialog } from "./QuitDialog";

const METER_DENOMINATORS = [1, 2, 4, 8, 16, 32, 64];

export { EXPORT_DIAGNOSTICS_EVENT } from "../stores/appEvents";
export { newProjectDialog, openFileDialog, saveAsFileDialog, saveFileDialog } from "../stores/fileDialogs";

/** Uses inline SVG icons for crisp rendering at all DPIs. */
export function Toolbar() {
  const [transportOptionsOpen, setTransportOptionsOpen] = createSignal(false);
  const [transportOptionsPos, setTransportOptionsPos] = createSignal({ left: 0, top: 0 });
  let transportOptionsRef: HTMLDivElement | undefined;
  let transportOptionsButton: HTMLButtonElement | undefined;

  /**
   * The transport strip scrolls horizontally, so a popover anchored inside it
   * would be clipped. Place it in the body at the button's rect instead.
   */
  function openTransportOptions() {
    if (transportOptionsOpen()) {
      setTransportOptionsOpen(false);
      return;
    }
    const rect = transportOptionsButton?.getBoundingClientRect();
    if (rect) {
      const width = 280;
      setTransportOptionsPos({
        left: Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)),
        top: rect.bottom + 6,
      });
    }
    setTransportOptionsOpen(true);
  }
  const [browseOpen, setBrowseOpen] = createSignal(false);
  const [timeSigOpen, setTimeSigOpen] = createSignal(false);
  const [numeratorDraft, setNumeratorDraft] = createSignal(4);
  const [denominatorDraft, setDenominatorDraft] = createSignal(4);
  const [fileMenuOpen, setFileMenuOpen] = createSignal(false);
  let timeSigMenuRef: HTMLDivElement | undefined;
  let fileMenuRef: HTMLDivElement | undefined;
  let preferencesButtonRef: HTMLButtonElement | undefined;
  let browseButtonRef: HTMLButtonElement | undefined;

  createEffect(() => {
    if (!transportOptionsOpen()) return;
    const outside = (event: PointerEvent) => {
      if (!transportOptionsRef?.contains(event.target as Node)) setTransportOptionsOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      setTransportOptionsOpen(false);
      transportOptionsButton?.focus();
    };
    window.addEventListener("pointerdown", outside);
    window.addEventListener("keydown", escape);
    onCleanup(() => {
      window.removeEventListener("pointerdown", outside);
      window.removeEventListener("keydown", escape);
    });
  });

  // Dismiss the file menu on outside pointerdown or Escape.
  createEffect(() => {
    if (!fileMenuOpen()) return;
    const dismiss = (event: Event) => {
      if (event instanceof PointerEvent && fileMenuRef?.contains(event.target as Node)) return;
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      setFileMenuOpen(false);
    };
    window.addEventListener("pointerdown", dismiss, true);
    window.addEventListener("keydown", dismiss, true);
    onCleanup(() => {
      window.removeEventListener("pointerdown", dismiss, true);
      window.removeEventListener("keydown", dismiss, true);
    });
  });

  // Dismiss the meter popover when clicking elsewhere in the DAW.
  createEffect(() => {
    if (!timeSigOpen()) return;
    const dismiss = (event: MouseEvent) => {
      if (timeSigMenuRef && !timeSigMenuRef.contains(event.target as Node)) setTimeSigOpen(false);
    };
    document.addEventListener("mousedown", dismiss);
    onCleanup(() => document.removeEventListener("mousedown", dismiss));
  });

  // The transport snapshot carries the click and the take; keep them in step.
  createEffect(() => {
    clickStore.adoptClick(store.transport().click as never);
    recordStore.adoptRecord(store.transport().record as never);
    store.adoptPunch(store.transport().punch);
  });

  const recordDisabled = () =>
    recordStore.pending() || (!recordStore.armed() && recordStore.disabledReason() !== null);
  const recordReason = () => {
    if (recordStore.pending()) return "";
    return recordStore.disabledReason() ?? (recordStore.armed() ? "" : i18n.t("toolbar.recordArmFirst"));
  };
  const toggleRecording = () => {
    if (recordStore.recording()) void recordStore.stop();
    else void recordStore.start();
  };

  // The punch fields show the live range, and a default one before it is set.
  const punchIn = () => store.punch().start_beats ?? 0;
  const punchOut = () => store.punch().end_beats ?? 8;

  const clickCountInValue = () => clickStore.countIn();
  const clickCountInReason = () =>
    clickStore.countInAvailable() ? "" : i18n.t("toolbar.countInNeedsArm");

  const toggleTimeSignature = () => {
    if (!timeSigOpen()) {
      setNumeratorDraft(store.project.properties.time_signature.numerator);
      setDenominatorDraft(store.project.properties.time_signature.denominator);
    }
    setTimeSigOpen((open) => !open);
  };

  const meterDraftValid = () => {
    const numerator = Math.trunc(numeratorDraft());
    const denominator = Math.trunc(denominatorDraft());
    return Number.isInteger(numerator) && numerator >= 1 && numerator <= 32
      && METER_DENOMINATORS.includes(denominator);
  };

  const applyTimeSignature = (event: Event) => {
    event.preventDefault();
    const numerator = Math.trunc(numeratorDraft());
    const denominator = Math.trunc(denominatorDraft());
    if (!meterDraftValid()) return;
    setTimeSigOpen(false);
    store.setTimeSignature(numerator, denominator);
  };

  // Both render buttons open the one export dialog; it owns the options.
  const handleRender = () => openExportAudioDialog("mix");

  const handleExportStems = () => openExportAudioDialog("stems");

  const playing = () => store.playbackPlaying();

  // The app-level shortcut requests the same action so the panel status updates.
  onMount(() => {
    const exportNow = () => void diagnosticsStore.exportBundle();
    window.addEventListener(EXPORT_DIAGNOSTICS_EVENT, exportNow);
    onCleanup(() => window.removeEventListener(EXPORT_DIAGNOSTICS_EVENT, exportNow));
  });

  const closePreferences = () => preferencesUi.close();

  const closeBrowse = () => {
    setBrowseOpen(false);
    queueMicrotask(() => browseButtonRef?.focus());
  };

  return (
    <div class="daw-toolbar" data-tauri-drag-region="deep">
      <div class="toolbar-scroll">
      {/* ── File operations ── */}
      <div class="toolbar-group file-group">
        <button class="toolbar-btn" aria-label={i18n.t("toolbar.newProject")} onClick={() => void newProjectDialog()} {...useTooltip(() => i18n.t("toolbar.newProject"))}>
          <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
            <path d="M3 1.5A1.5 1.5 0 014.5 0h4.293a1.5 1.5 0 011.06.44l2.208 2.207a1.5 1.5 0 01.439 1.06V13.5A1.5 1.5 0 0111 15H4.5A1.5 1.5 0 013 13.5v-12z" stroke="currentColor" stroke-width="1.2" fill="none"/>
            <path d="M7.5 5v5M5 7.5h5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
          </svg>
        </button>
        <button class="toolbar-btn" aria-label={i18n.t("toolbar.openFile")} onClick={openFileDialog} {...useTooltip(() => i18n.t("toolbar.openFile"))}>
          <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
            <path d="M2.5 2h3.382a1 1 0 01.894.553L7.5 4h5a1 1 0 011 1v7.5a1 1 0 01-1 1h-10a1 1 0 01-1-1V3a1 1 0 011-1z" stroke="currentColor" stroke-width="1.2" fill="none"/>
          </svg>
        </button>
        <button class="toolbar-btn" aria-label={i18n.t("toolbar.save")} onClick={saveFileDialog} {...useTooltip(() => i18n.t("toolbar.save"))}>
          <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
            <path d="M2 1.5A1.5 1.5 0 013.5 0h7.586a1.5 1.5 0 011.06.44l1.415 1.414a1.5 1.5 0 01.439 1.06V13.5A1.5 1.5 0 0112.5 15h-9A1.5 1.5 0 012 13.5v-12z" stroke="currentColor" stroke-width="1.2" fill="none"/>
            <path d="M5 0v3.5a.5.5 0 00.5.5h4a.5.5 0 00.5-.5V0" stroke="currentColor" stroke-width="1.2"/>
            <rect x="4" y="9" width="7" height="4" rx="0.5" stroke="currentColor" stroke-width="1.2" fill="none"/>
          </svg>
        </button>
        <div class="toolbar-menu" ref={fileMenuRef}>
          <button
            class="toolbar-btn text-btn"
            classList={{ active: fileMenuOpen() }}
            aria-label={i18n.t("toolbar.fileMenu")}
            aria-haspopup="menu"
            aria-expanded={fileMenuOpen()}
            onClick={() => setFileMenuOpen((open) => !open)}
            {...useTooltip(() => i18n.t("toolbar.fileMenu"))}
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
              <path d="M2.5 4.5L6 8l3.5-3.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          </button>
          <Show when={fileMenuOpen()}>
            <div class="preferences-popover toolbar-menu-popover" role="menu">
              <button class="toolbar-menu-item" role="menuitem" onClick={() => { setFileMenuOpen(false); workspaceStore.openChooser(); }}>
                {i18n.t("workspace.switch")}
              </button>
              <div class="preferences-divider" />
              <button class="toolbar-menu-item" role="menuitem" onClick={() => { setFileMenuOpen(false); void saveAsFileDialog(); }}>
                {i18n.t("toolbar.saveAs")}
                <kbd>{commandBindingLabel("file.saveAs")}</kbd>
              </button>
              <div class="preferences-divider" />
              <button
                class="toolbar-menu-item"
                role="menuitem"
                disabled={!recoveryStore.available()}
                title={recoveryStore.available() ? undefined : i18n.t("recovery.menuDisabled")}
                onClick={() => { setFileMenuOpen(false); void recoveryStore.openDialog(); }}
              >
                {i18n.t("recovery.menu")}
              </button>
              <div class="preferences-divider" />
              <button class="toolbar-menu-item" role="menuitem" onClick={() => { setFileMenuOpen(false); void openMidiImportDialog(); }}>
                {i18n.t("toolbar.midiImport")}
              </button>
              <button class="toolbar-menu-item" role="menuitem" onClick={() => { setFileMenuOpen(false); void openAlignedStemsDialog(); }}>
                {i18n.t("command.file.importAlignedStems")}
              </button>
              <button class="toolbar-menu-item" role="menuitem" onClick={() => { setFileMenuOpen(false); openMidiExportDialog(); }}>
                {i18n.t("toolbar.midiExport")}
              </button>
              <button class="toolbar-menu-item export-audio-menu-item" role="menuitem" onClick={() => { setFileMenuOpen(false); openExportAudioDialog("mix"); }}>
                {i18n.t("toolbar.exportAudio")}
                <kbd>{commandBindingLabel("file.exportAudio")}</kbd>
              </button>
            </div>
          </Show>
        </div>
      </div>

      <div class="toolbar-separator" />

      {/* ── Transport ── */}
      <div class="toolbar-group transport-group">
        <button
          class="toolbar-btn transport-btn stop"
          aria-label={i18n.t("toolbar.stop")}
          onClick={() => store.stop()}
          {...useTooltip(() => i18n.t("toolbar.stop"))}
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
            <rect x="1" y="1" width="10" height="10" rx="1.5"/>
          </svg>
        </button>
        <button
          class="toolbar-btn transport-btn play"
          classList={{ active: playing() }}
          aria-label={i18n.t("toolbar.playPause")}
          onClick={() => store.togglePlayPause()}
          {...useTooltip(() => i18n.t("toolbar.playPause"))}
        >
          {playing() ? (
            <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
              <rect x="2" y="1" width="3" height="10" rx="1"/>
              <rect x="7" y="1" width="3" height="10" rx="1"/>
            </svg>
          ) : (
            <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
              <path d="M2.5 1.116a1 1 0 011.5-.866l7 4.04a1 1 0 010 1.732l-7 4.04a1 1 0 01-1.5-.866V1.116z"/>
            </svg>
          )}
        </button>
        <button
          class="toolbar-btn transport-btn loop"
          classList={{ active: store.loop().enabled }}
          aria-label={i18n.t("transport.loop")}
          aria-pressed={store.loop().enabled}
          aria-busy={store.loopPending()}
          disabled={!store.loopRegion()}
          title={store.loopRegion() ? undefined : i18n.t("transport.loopNeedsRegion")}
          onClick={() => void store.toggleLoop()}
          {...useTooltip(() => store.loopRegion()
            ? i18n.t("transport.loopHint")
            : i18n.t("transport.loopNeedsRegion"))}
        >
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
            <path d="M12 4H5a3 3 0 0 0-3 3v1m2 4h7a3 3 0 0 0 3-3V8M10 2l2 2-2 2M6 10l-2 2 2 2"/>
          </svg>
        </button>

        <button
          class="toolbar-btn transport-btn record"
          classList={{ active: recordStore.recording() }}
          aria-pressed={recordStore.recording()}
          aria-label={i18n.t("toolbar.record")}
          disabled={recordDisabled()}
          onClick={toggleRecording}
          {...useTooltip(() => recordReason() || i18n.t("toolbar.record"))}
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
            <circle cx="6" cy="6" r="4.5"/>
          </svg>
        </button>
        <Show when={recordStore.recording()}>
          <span class="record-status" role="status">
            {i18n.t("toolbar.recording", { time: recordStore.recordingLabel() })}
          </span>
          <button
            class="toolbar-btn text-btn record-stop"
            aria-label={i18n.t("toolbar.recordStop")}
            onClick={() => void recordStore.stop()}
          >
            {i18n.t("toolbar.recordStop")}
          </button>
        </Show>
        {/* Which take a press starts: MIDI into a pattern, or the input. */}
        <Show when={recordStore.armLine()}>
          {(line) => (
            <span class="record-arm-chip" role="status" data-record-arm-chip>
              {line()}
            </span>
          )}
        </Show>

        <button
          class="toolbar-btn transport-btn click"
          classList={{ active: clickStore.enabled() }}
          aria-pressed={clickStore.enabled()}
          aria-label={i18n.t("toolbar.click")}
          disabled={clickStore.pending()}
          onClick={() => void clickStore.setEnabled(!clickStore.enabled())}
          {...useTooltip(() => i18n.t("toolbar.click"))}
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
            <path d="M6 0l1.2 4.2L11 5.4 7.4 7.2 8 12 6 9.4 4 12l.6-4.8L1 5.4l3.8-1.2L6 0z"/>
          </svg>
        </button>
        <div class="transport-options" ref={transportOptionsRef}>
          <button ref={transportOptionsButton} class="toolbar-btn transport-btn transport-options-toggle"
            aria-label={i18n.t("toolbar.transportSettings")} aria-expanded={transportOptionsOpen()}
            aria-haspopup="dialog" onClick={openTransportOptions}
            {...useTooltip(() => i18n.t("toolbar.transportSettings"))}>
            <svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor" aria-hidden="true"><circle cx="3" cy="7" r="1"/><circle cx="7" cy="7" r="1"/><circle cx="11" cy="7" r="1"/></svg>
          </button>
          <Show when={transportOptionsOpen()}>
            <Portal>
            <div
              ref={transportOptionsRef}
              class="transport-options-popover portalled"
              role="dialog"
              aria-label={i18n.t("toolbar.transportSettings")}
              style={{
                left: `${transportOptionsPos().left}px`,
                top: `${transportOptionsPos().top}px`,
              }}
            >
              <strong>{i18n.t("toolbar.transportSettings")}</strong>
              <label class="transport-option-field">
                <span>{i18n.t("toolbar.countIn")}</span>
                <select
                  class="toolbar-select click-count-in"
                  aria-label={i18n.t("toolbar.countIn")}
                  value={clickCountInValue()}
                  disabled={!clickStore.countInAvailable() || clickStore.pending()}
                  title={clickCountInReason() ?? undefined}
                  onChange={(event) =>
                    void clickStore.setCountIn(event.currentTarget.value as never)
                  }
                  {...useTooltip(() => i18n.t("toolbar.countIn"))}
                >
                  <option value="off">{i18n.t("toolbar.countInOff")}</option>
                  <option value="one_bar">{i18n.t("toolbar.countInOneBar")}</option>
                  <option value="two_bars">{i18n.t("toolbar.countInTwoBars")}</option>
                </select>
              </label>
              <label class="transport-option-field">
                <span>{i18n.t("toolbar.clickLevel")}</span>
                <input
                  type="number"
                  class="click-level-input"
                  aria-label={i18n.t("toolbar.clickLevel")}
                  title={i18n.t("toolbar.clickLevel")}
                  min="-60"
                  max="6"
                  step="1"
                  value={clickStore.levelDb()}
                  disabled={clickStore.pending()}
                  onChange={(event) => void clickStore.setLevelDb(Number(event.currentTarget.value))}
                />
              </label>
              <Show when={clickCountInReason()}>
                <span class="click-count-in-reason">{clickCountInReason()}</span>
              </Show>
              <label class="transport-option-field">
                <span>{i18n.t("toolbar.quantise")}</span>
                <select
                  class="toolbar-select record-quantise"
                  aria-label={i18n.t("toolbar.quantise")}
                  value={recordStore.quantise()}
                  disabled={recordStore.pending()}
                  onChange={(event) => void recordStore.setQuantise(event.currentTarget.value as never)}
                  {...useTooltip(() => i18n.t("toolbar.quantise"))}
                >
                  <option value="off">{i18n.t("toolbar.quantiseOff")}</option>
                  <option value="thirty_second">1/32</option>
                  <option value="sixteenth">1/16</option>
                  <option value="eighth">1/8</option>
                  <option value="quarter">1/4</option>
                </select>
              </label>
              <div class="transport-option-field">
                <span>{i18n.t("toolbar.overdub")}</span>
                <button
                  class="toolbar-btn text-btn record-overdub"
                  classList={{ active: recordStore.overdub() }}
                  aria-pressed={recordStore.overdub()}
                  aria-label={i18n.t("toolbar.overdub")}
                  disabled={recordStore.pending()}
                  onClick={() => void recordStore.setOverdub(!recordStore.overdub())}
                  {...useTooltip(() => i18n.t("toolbar.overdubHint"))}
                >
                  {i18n.t("toolbar.overdub")}
                </button>
              </div>
              <div class="transport-option-field">
                <span>{i18n.t("toolbar.punch")}</span>
                <span class="record-punch" data-record-punch>
                  <button
                    class="toolbar-btn text-btn record-punch-toggle"
                    classList={{ active: store.punch().enabled }}
                    aria-pressed={store.punch().enabled}
                    aria-label={i18n.t("toolbar.punchToggle")}
                    disabled={store.punchPending()}
                    onClick={() =>
                      void store.setPunchRegion(punchIn(), punchOut(), !store.punch().enabled)}
                    {...useTooltip(() => i18n.t("toolbar.punchHint"))}
                  >
                    {i18n.t("toolbar.punch")}
                  </button>
                  <input
                    type="number"
                    class="punch-input record-punch-in"
                    aria-label={i18n.t("toolbar.punchIn")}
                    title={i18n.t("toolbar.punchIn")}
                    min="0"
                    max="4096"
                    step="1"
                    value={punchIn()}
                    disabled={store.punchPending()}
                    onChange={(event) =>
                      void store.setPunchRegion(Number(event.currentTarget.value), punchOut(), store.punch().enabled)}
                  />
                  <input
                    type="number"
                    class="punch-input record-punch-out"
                    aria-label={i18n.t("toolbar.punchOut")}
                    title={i18n.t("toolbar.punchOut")}
                    min="0"
                    max="4096"
                    step="1"
                    value={punchOut()}
                    disabled={store.punchPending()}
                    onChange={(event) =>
                      void store.setPunchRegion(punchIn(), Number(event.currentTarget.value), store.punch().enabled)}
                  />
                </span>
              </div>
              <Show when={store.punchError()}>
                <span class="record-punch-error" role="status">
                  {i18n.t("punch.failed", { message: store.punchError() ?? "" })}
                </span>
              </Show>
              <Show when={recordReason()}><p class="record-reason">{recordReason()}</p></Show>
              <div class="transport-option-field">
                <span>{i18n.t("automation.write.arm")}</span>
                <button
                  class="toolbar-btn text-btn automation-write"
                  classList={{ active: automationWriteArmed() }}
                  aria-pressed={automationWriteArmed()}
                  aria-label={i18n.t("automation.write.arm")}
                  onClick={() => toggleAutomationWriteArm()}
                  {...useTooltip(() => i18n.t("automation.write.armHint"))}
                >
                  {i18n.t("automation.write.arm")}
                </button>
              </div>
            </div>
            </Portal>
          </Show>
        </div>
      </div>

      <Show when={store.loopError()}>
        <div class="transport-status" role="status" aria-live="polite">
          {i18n.t("loop.failed", { message: store.loopError() ?? "" })}
        </div>
      </Show>

      <Show when={recordStore.error() || recordStore.report()}>
        <span class="record-report selectable">
          {recordStore.error() ?? recordStore.report()}
        </span>
      </Show>

      <div class="toolbar-separator" />

      {/* ── LCD readout: position, tempo, meter ── */}
      <div class="toolbar-lcd">
      <div class="position-display">
        {formatBeats(store.scrubPosition() ?? (store.auditionPositionSeconds() !== null ? projectClock(store.project.properties).quarterAtSeconds(store.auditionPositionSeconds()!) : store.previewCursorBeats() ?? store.transport().position_beats), store.project.properties.time_signature.numerator)}
      </div>

      {/* ── Tempo + time sig ── */}
      <div class="toolbar-group tempo-group">
        <input
          type="number"
          class="tempo-input"
          aria-label={i18n.t("toolbar.tempo")}
          value={store.project.properties.tempo}
          min="20"
          max="999"
          step="1"
          onChange={(e) => {
            const v = parseFloat(e.currentTarget.value);
            if (v >= 20 && v <= 999) store.setTempo(v);
          }}
          {...useTooltip(() => i18n.t("toolbar.tempo"))}
        />
        <span class="tempo-unit">BPM</span>
        <div class="time-sig-menu" ref={timeSigMenuRef}>
          <button
            class="time-sig-display"
            classList={{ active: timeSigOpen() }}
            onClick={toggleTimeSignature}
            {...useTooltip(() => i18n.t("toolbar.timeSignature"))}
            aria-haspopup="dialog"
            aria-expanded={timeSigOpen()}
          >
            {store.project.properties.time_signature.numerator}/{store.project.properties.time_signature.denominator}
          </button>
          <Show when={timeSigOpen()}>
            <form
              class="time-sig-popover"
              role="dialog"
              aria-label={i18n.t("toolbar.timeSignature")}
              onSubmit={applyTimeSignature}
              onKeyDown={(e) => {
                if (e.key === "Escape") setTimeSigOpen(false);
              }}
            >
              <div class="time-sig-fields">
                <label class="time-sig-field">
                  <span>{i18n.t("toolbar.numerator")}</span>
                  <input
                    type="number"
                    min="1"
                    max="32"
                    step="1"
                    value={numeratorDraft()}
                    onInput={(e) => setNumeratorDraft(parseInt(e.currentTarget.value, 10) || 0)}
                  />
                </label>
                <span class="time-sig-slash">/</span>
                <label class="time-sig-field">
                  <span>{i18n.t("toolbar.denominator")}</span>
                  <select
                    value={denominatorDraft()}
                    onChange={(e) => setDenominatorDraft(parseInt(e.currentTarget.value, 10))}
                  >
                    <For each={METER_DENOMINATORS}>
                      {(value) => <option value={value}>{value}</option>}
                    </For>
                  </select>
                </label>
                <button class="toolbar-btn text-btn time-sig-apply" type="submit" disabled={!meterDraftValid()}>
                  {i18n.t("toolbar.apply")}
                </button>
              </div>
              <p class="time-sig-hint" classList={{ error: !meterDraftValid() }} role={meterDraftValid() ? undefined : "alert"}>
                {meterDraftValid() ? i18n.t("toolbar.timeSignatureHint") : i18n.t("toolbar.timeSignatureInvalid")}
              </p>
            </form>
          </Show>
        </div>
      </div>
      </div>

      <div class="toolbar-separator" />

      {/* ── Render ── */}
      <div class="toolbar-group render-group">
      <button
        class="toolbar-btn render-btn"
        aria-label={i18n.t("toolbar.renderWav")}
        onClick={handleRender}
        {...useTooltip(() => i18n.t("toolbar.renderWav"))}
      >
        <svg width="15" height="15" viewBox="0 0 14 14" fill="none">
          <path d="M1 7h2.2M5 3.2v7.6M8 1.4v11.2M11 4.6v4.8M13 7h-.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
        </svg>
      </button>
      <button
        class="toolbar-btn"
        aria-label={i18n.t("toolbar.renderStems")}
        onClick={handleExportStems}
        {...useTooltip(() => i18n.t("toolbar.renderStems"))}
      >
        <svg width="15" height="15" viewBox="0 0 14 14" fill="none">
          <path d="M2 3.5h10M2 7h10M2 10.5h6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
        </svg>
      </button>
      <button
        class="toolbar-btn midi-toolbar-import"
        aria-label={i18n.t("toolbar.midiImport")}
        onClick={() => void openMidiImportDialog()}
        {...useTooltip(() => i18n.t("toolbar.midiImport"))}
      >
        <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
          <circle cx="3.5" cy="11.8" r="1.9" fill="currentColor"/>
          <path d="M5.4 11.8V4l3.2-1v7" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
          <path d="M12 2.2v8.4M9.8 8.4l2.2 2.2 2.2-2.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </button>
      <button
        class="toolbar-btn midi-toolbar-export"
        aria-label={i18n.t("toolbar.midiExport")}
        onClick={() => openMidiExportDialog()}
        {...useTooltip(() => i18n.t("toolbar.midiExport"))}
      >
        <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
          <circle cx="3.5" cy="11.8" r="1.9" fill="currentColor"/>
          <path d="M5.4 11.8V4l3.2-1v7" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
          <path d="M12 11.8V3.4M9.8 5.6L12 3.4l2.2 2.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </button>
      </div>

      <div class="toolbar-separator" />

      {/* ── Sidebar toggles ── */}
      <div class="toolbar-group workspace-group">
      <button
        class="toolbar-btn text-btn dsl-btn"
        classList={{ active: panelStore.sidebarOpen() && panelStore.sidebarTab() === "dsl" }}
        aria-label={i18n.t("toolbar.dslEditor")}
        onClick={() => panelStore.toggleSidebar("dsl")}
        {...useTooltip(() => i18n.t("toolbar.dslEditor"))}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
          <rect x="1.5" y="2" width="11" height="10" rx="1.5" stroke="currentColor" stroke-width="1.2"/>
          <path d="M5 2v10" stroke="currentColor" stroke-width="1.2"/>
          <path d="M7 5.4h3.4M7 8h2.4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
        </svg>
      </button>

      <button
        class="toolbar-btn text-btn ai-btn"
        classList={{ active: panelStore.aiOpen() }}
        aria-label={i18n.t("toolbar.aiAssistant")}
        onClick={() => panelStore.toggleAi()}
        {...useTooltip(() => i18n.t("toolbar.aiAssistant"))}
      >AI</button>
      </div>

      {/* ── Spacer ── */}
      <div style="flex: 1" />
      </div>

    <div class="toolbar-actions">
      <button
        class="toolbar-btn text-btn preferences-btn"
        ref={preferencesButtonRef}
        aria-label={i18n.t("toolbar.preferences")}
        classList={{ active: preferencesUi.open() }}
        aria-haspopup="dialog"
        aria-expanded={preferencesUi.open()}
        onClick={() => {
          if (preferencesUi.open()) {
            void preferencesUi.requestClose(closePreferences);
          } else {
            preferencesUi.openOn("display", "connection", preferencesButtonRef);
          }
        }}
        {...useTooltip(() => i18n.t("toolbar.preferences"))}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
          <path d="M1.6 4.2h10.8M1.6 9.8h10.8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
          <circle cx="5.2" cy="4.2" r="1.6" fill="var(--bg-primary)" stroke="currentColor" stroke-width="1.2"/>
          <circle cx="8.8" cy="9.8" r="1.6" fill="var(--bg-primary)" stroke="currentColor" stroke-width="1.2"/>
        </svg>
        <span class="toolbar-btn-label">{i18n.t("toolbar.preferences")}</span>
      </button>
      <button
        class="toolbar-btn text-btn browse-btn"
        ref={browseButtonRef}
        aria-label={i18n.t("toolbar.browse")}
        classList={{ active: browseOpen() }}
        aria-haspopup="dialog"
        aria-expanded={browseOpen()}
        onClick={() => setBrowseOpen((open) => !open)}
        {...useTooltip(() => i18n.t("toolbar.browse"))}
      >
        <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
          <rect x="1.6" y="1.6" width="4.3" height="4.3" rx="1" stroke="currentColor" stroke-width="1.2"/>
          <rect x="8.1" y="1.6" width="4.3" height="4.3" rx="1" stroke="currentColor" stroke-width="1.2"/>
          <rect x="1.6" y="8.1" width="4.3" height="4.3" rx="1" stroke="currentColor" stroke-width="1.2"/>
          <rect x="8.1" y="8.1" width="4.3" height="4.3" rx="1" stroke="currentColor" stroke-width="1.2"/>
        </svg>
        <span class="toolbar-btn-label">{i18n.t("toolbar.browse")}</span>
      </button>
    </div>

    <PreferencesHost onClose={closePreferences} />
    <ResourceBrowser open={browseOpen()} onClose={closeBrowse} />

    <WindowControls />

    {/* The Open/New guard: one modal for both, with the quit prompt's answers. */}
    <Show when={store.unsavedPrompt()}>
      <QuitDialog
        body={i18n.t("dialog.unsavedOpenBody", { project: store.project.name || i18n.t("status.untitled") })}
        onSave={() => void store.saveForUnsavedPrompt()}
        onDiscard={() => store.answerUnsavedPrompt("discard")}
        onCancel={() => store.answerUnsavedPrompt("cancel")}
      />
    </Show>
  </div>
  );
}

// ============================================================================
// Helpers
// ============================================================================

function formatBeats(beats: number, _beatsPerBar: number): string {
  const { bar, beat, subdivision } = projectClock(store.project.properties).barBeatAt(beats);
  return `${bar}:${beat}:${subdivision}`;
}
