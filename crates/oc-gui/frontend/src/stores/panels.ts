/**
 * Panel layout state — split-view architecture.
 *
 * Layout:
 *   Top:    Timeline (always visible, variable height ratio)
 *   Bottom: Switchable views, all linked by the selected pattern/clip:
 *     C = Clips     — list all clips/patterns, click to select
 *     R = Piano Roll — note editor for selected pattern
 *     P = Presets    — preset browser for selected pattern's instrument
 *     M = Mixer      — bus graph + selected node detail
 *     D = Detail     — (no shortcut) split: pattern attrs+preset left, piano roll right
 *   Right: Sidebar with tabs (DSL, Plugins, Store, AI)
 *
 * Selection model:
 *   Selecting a pattern (in C list, in Timeline, or anywhere) syncs
 *   all bottom views. Detail view follows selection too.
 *
 * Interactions:
 *   C / R / P / M = toggle bottom view
 *   Double-click clip/pattern in Timeline = open Detail view
 *   Right-click track > "Create pattern here" = create + open Detail
 *   Escape = close bottom
 */
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { log } from "./log";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Bottom panel view modes */
export type BottomMode = "clips" | "pianoroll" | "presets" | "mixer" | "drumpads" | "samples" | "detail";

/** Region that starts playback when no preview owns audio; set by pointer or keyboard focus. */
export type EditingRegion = "timeline" | "bottom";

/** What Space does: audition the bottom panel's selection or drive the timeline. */
export type SpaceCommand =
  | { kind: "audition-preset"; preset: string }
  | { kind: "audition-pattern"; pattern: string }
  | { kind: "toggle-audition" }
  | { kind: "transport" };

/** Right sidebar tabs */
export type SidebarPanelId = "dsl" | "plugins" | "store" | "ai";

/** All identifiers (union for floating support) */
export type PanelId = BottomMode | SidebarPanelId;

export const BOTTOM_MODES: BottomMode[] = ["clips", "pianoroll", "presets", "mixer", "drumpads", "samples", "detail"];
export const SIDEBAR_PANEL_IDS: SidebarPanelId[] = ["dsl", "plugins", "store", "ai"];

export const ALL_PANEL_IDS: PanelId[] = [...BOTTOM_MODES, ...SIDEBAR_PANEL_IDS];

export const PANEL_LABELS: Record<PanelId, string> = {
  clips: "Clips",
  pianoroll: "Piano Roll",
  presets: "Presets",
  mixer: "Mixer",
  drumpads: "Drum Kit",
  samples: "Samples",
  detail: "Detail",
  dsl: "DSL",
  plugins: "Plugins",
  store: "Store",
  ai: "AI",
};

/** Keyboard shortcut keys for toggling bottom modes (Detail has no shortcut) */
export const BOTTOM_KEYS: Partial<Record<BottomMode, string>> = {
  clips: "C",
  pianoroll: "R",
  presets: "P",
  mixer: "M",
  drumpads: "K",
};

// ---------------------------------------------------------------------------
// Floating panel state
// ---------------------------------------------------------------------------

export interface FloatingPanelState {
  id: PanelId;
  x: number;
  y: number;
  width: number;
  height: number;
  minimized: boolean;
  zIndex: number;
}

/** Height of `.floating-panel-title` in `chrome.css`. */
const PANEL_TITLE_HEIGHT = 32;
/** Strip of the title bar that stays inside the window when a panel is dragged out. */
const PANEL_MIN_VISIBLE = 40;

/**
 * Keep the title bar — the only re-dock handle — inside the window: a
 * `PANEL_MIN_VISIBLE` px wide strip of it stays visible horizontally, and the
 * whole (shorter) bar stays visible vertically.
 */
function clampPanelPosition(x: number, y: number, width: number): { x: number; y: number } {
  const visibleY = Math.min(PANEL_MIN_VISIBLE, PANEL_TITLE_HEIGHT);
  return {
    x: Math.max(PANEL_MIN_VISIBLE - width, Math.min(window.innerWidth - PANEL_MIN_VISIBLE, x)),
    y: Math.max(0, Math.min(window.innerHeight - visibleY, y)),
  };
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

// Bottom split
const [bottomMode, setBottomMode] = createSignal<BottomMode | null>(null);
const [activeRegion, setActiveRegion] = createSignal<EditingRegion>("timeline");
const [splitRatio, setSplitRatio] = createSignal(0.55);
const [targetSplitRatio, setTargetSplitRatio] = createSignal(0.55);
const [isDividerDragging, setIsDividerDragging] = createSignal(false);

// Right sidebar
const [sidebarOpen, setSidebarOpen] = createSignal(false);
const [sidebarTab, setSidebarTab] = createSignal<SidebarPanelId>("plugins");
const [sidebarWidth, setSidebarWidth] = createSignal(320);

// Floating panels
const [floatingPanels, setFloatingPanels] = createStore<FloatingPanelState[]>([]);
let nextZ = 20;

// ---------------------------------------------------------------------------
// Bottom Panel Actions
// ---------------------------------------------------------------------------

/**
 * Space toggles the current preview owner. Without a preview, the active region
 * selects either its instrument/pattern or the arrangement transport.
 */
export function spaceCommand(
  region: EditingRegion,
  mode: BottomMode | null,
  selectedPreset: string | null,
  selectedPattern: string | null,
  auditionActive: boolean,
): SpaceCommand {
  if (auditionActive) return { kind: "toggle-audition" };
  if (region !== "bottom") return { kind: "transport" };
  if (mode === "presets" && selectedPreset) {
    return { kind: "audition-preset", preset: selectedPreset };
  }
  if (
    (mode === "clips" || mode === "pianoroll" || mode === "detail") &&
    selectedPattern
  ) {
    return { kind: "audition-pattern", pattern: selectedPattern };
  }
  return { kind: "transport" };
}

function openBottom(mode: BottomMode, ratio?: number) {
  const current = bottomMode();
  if (current === mode) return;
  log.info("panels", `Bottom → ${mode}`);
  setBottomMode(mode);
  if (ratio !== undefined) {
    setTargetSplitRatio(ratio);
    setSplitRatio(ratio);
  } else if (current === null) {
    // Opening from collapsed
    setTargetSplitRatio(0.55);
    setSplitRatio(0.55);
  }
}

function closeBottom() {
  if (bottomMode() === null) return;
  log.info("panels", "Bottom → closed");
  setBottomMode(null);
}

function toggleBottom(mode: BottomMode) {
  if (bottomMode() === mode) closeBottom();
  else openBottom(mode);
}

/**
 * Open the Detail view for editing a pattern.
 * Called when user double-clicks a clip/pattern or creates a new pattern.
 * Animates to a 45/55 split (more room for the editor).
 */
function openDetail() {
  const wasNull = bottomMode() === null;
  log.info("panels", "Bottom → detail");
  setBottomMode("detail");
  // Give more room to the detail view when opening fresh or from a large top area
  if (wasNull || splitRatio() > 0.5) {
    setTargetSplitRatio(0.4);
    setSplitRatio(0.4);
  }
}

// ---------------------------------------------------------------------------
// Right Sidebar Actions
// ---------------------------------------------------------------------------

function openSidebar(tab?: SidebarPanelId) {
  if (tab) setSidebarTab(tab);
  setSidebarOpen(true);
}

function closeSidebar() {
  setSidebarOpen(false);
}

function toggleSidebar(tab?: SidebarPanelId) {
  if (sidebarOpen() && (!tab || sidebarTab() === tab)) closeSidebar();
  else openSidebar(tab);
}

function selectSidebarTab(tab: SidebarPanelId) {
  setSidebarTab(tab);
  if (!sidebarOpen()) setSidebarOpen(true);
}

// ---------------------------------------------------------------------------
// Divider Drag
// ---------------------------------------------------------------------------

function startDividerDrag() { setIsDividerDragging(true); }

/** A drag is not an animated target: only the live ratio moves. */
function updateSplitRatio(ratio: number) {
  setSplitRatio(Math.max(0.2, Math.min(0.85, ratio)));
}

function endDividerDrag() {
  setTargetSplitRatio(splitRatio());
  setIsDividerDragging(false);
}

// ---------------------------------------------------------------------------
// Floating (detach/dock any panel)
// ---------------------------------------------------------------------------

function detachPanel(id: PanelId) {
  if (floatingPanels.find(p => p.id === id)) {
    log.warn("panels", `"${id}" already floating`);
    return;
  }
  log.info("panels", `Detaching "${id}" to floating`);
  nextZ++;
  const count = floatingPanels.length;
  const width = Math.min(900, window.innerWidth - 200);
  const height = Math.min(500, window.innerHeight - 200);
  const position = clampPanelPosition(80 + count * 30, 60 + count * 30, width);
  const fp: FloatingPanelState = {
    id,
    x: position.x,
    y: position.y,
    width,
    height,
    minimized: false,
    zIndex: nextZ,
  };
  setFloatingPanels([...floatingPanels, fp]);

  // If we detached the active bottom mode, close bottom
  if (BOTTOM_MODES.includes(id as BottomMode) && bottomMode() === id) {
    setBottomMode(null);
  }
}

function dockPanel(id: PanelId) {
  log.info("panels", `Docking "${id}"`);
  setFloatingPanels(floatingPanels.filter(p => p.id !== id));

  if (BOTTOM_MODES.includes(id as BottomMode)) {
    openBottom(id as BottomMode);
  } else if (SIDEBAR_PANEL_IDS.includes(id as SidebarPanelId)) {
    selectSidebarTab(id as SidebarPanelId);
  }
}

/**
 * Close the surface a panel is showing: a floating panel dismisses its own
 * window, a docked one closes the surface that hosts it.
 */
function closePanel(id: PanelId) {
  if (isFloating(id)) {
    log.info("panels", `Closing floating "${id}"`);
    setFloatingPanels(floatingPanels.filter(p => p.id !== id));
    return;
  }
  if (BOTTOM_MODES.includes(id as BottomMode)) closeBottom();
  else closeSidebar();
}

function movePanel(id: PanelId, x: number, y: number) {
  setFloatingPanels((p) => p.id === id, produce((p) => {
    const position = clampPanelPosition(x, y, p.width);
    p.x = position.x;
    p.y = position.y;
  }));
}

/** Re-clamp every floating panel after the window changed size. */
function reclampFloatingPanels() {
  setFloatingPanels(produce((panels) => {
    for (const panel of panels) {
      const position = clampPanelPosition(panel.x, panel.y, panel.width);
      panel.x = position.x;
      panel.y = position.y;
    }
  }));
}

function resizePanel(id: PanelId, width: number, height: number) {
  setFloatingPanels((p) => p.id === id, produce((p) => {
    p.width = Math.max(200, width);
    p.height = Math.max(100, height);
  }));
}

function toggleMinimize(id: PanelId) {
  setFloatingPanels((p) => p.id === id, "minimized", (m) => !m);
}

function bringToFront(id: PanelId) {
  nextZ++;
  setFloatingPanels((p) => p.id === id, "zIndex", nextZ);
}

function isFloating(id: PanelId): boolean {
  return floatingPanels.some(p => p.id === id);
}

function getFloating(id: PanelId): FloatingPanelState | undefined {
  return floatingPanels.find(p => p.id === id);
}

// ---------------------------------------------------------------------------
// Legacy compat shims
// ---------------------------------------------------------------------------

function getPanel(id: PanelId) {
  const fp = getFloating(id);
  if (fp) return { ...fp, mode: "floating" as const };
  return { id, mode: "docked" as const, x: 0, y: 0, width: 800, height: 400, minimized: false, zIndex: 0 };
}

function selectDockedTab(id: PanelId) {
  if (BOTTOM_MODES.includes(id as BottomMode)) openBottom(id as BottomMode);
  else if (SIDEBAR_PANEL_IDS.includes(id as SidebarPanelId)) selectSidebarTab(id as SidebarPanelId);
}

const activeDockedTab = () => bottomMode() ?? sidebarTab();
const aiOpen = () => sidebarOpen() && sidebarTab() === "ai";
function toggleAi() { toggleSidebar("ai"); }

// `bottomPanel()` reads as "which docked panel is open" at call sites that only test for null.
const bottomPanel = bottomMode;

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

export const panelStore = {
  // Bottom split
  bottomMode,
  bottomPanel, // alias
  activeRegion,
  setActiveRegion,
  spaceCommand,
  splitRatio,
  targetSplitRatio,
  isDividerDragging,
  openBottom,
  closeBottom,
  toggleBottom,
  openDetail,
  startDividerDrag,
  updateSplitRatio,
  endDividerDrag,

  // Right sidebar
  sidebarOpen,
  sidebarTab,
  sidebarWidth,
  setSidebarWidth,
  openSidebar,
  closeSidebar,
  toggleSidebar,
  selectSidebarTab,

  // Floating
  floatingPanels,
  detachPanel,
  dockPanel,
  closePanel,
  movePanel,
  reclampFloatingPanels,
  resizePanel,
  toggleMinimize,
  bringToFront,
  isFloating,
  getFloating,

  // Legacy compat
  panels: [] as PanelId[],
  activeDockedTab,
  aiOpen,
  toggleAi,
  selectDockedTab,
  getPanel,
  ALL_PANEL_IDS,
  PANEL_LABELS,
};
