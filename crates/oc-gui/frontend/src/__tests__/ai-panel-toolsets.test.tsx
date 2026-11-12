/**
 * Toolset surface: the settings modal lists one compact row per set with its
 * tools on hover, and the closed panel keeps a thumbnail of the enabled sets.
 * Selection `null` means the whole surface (a backend without toolsets), `[]`
 * means the user enabled nothing.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { TooltipLayer } from "../components/Tooltip";

const SETS = [
  { id: "dsl-edit", title: "DSL edit", description: "Read a subtree and propose one whole-subtree replacement", tools: ["edit_subtree", "project_get"], default_on: true },
  { id: "project-edit", title: "Project edit", description: "Tracks, presets, effects, buses, undo and transactions", tools: ["tracks_list"], default_on: true },
  { id: "listen", title: "Listen", description: "Render a range and measure it", tools: ["render_range"], default_on: true },
  { id: "plugins", title: "Plugins", description: "Search the plugin catalog and read loaded parameters", tools: ["plugin_search"], default_on: true },
  { id: "transport", title: "Transport", description: "Move and query the playhead", tools: ["transport_play"], default_on: false },
];

const TOOLS = [
  { name: "edit_subtree", description: "Propose one complete subtree replacement", area: "project", inputSchema: {} },
  { name: "project_get", description: "", area: "project", inputSchema: {} },
  { name: "tracks_list", description: "List tracks and their mix", area: "project", inputSchema: {} },
  { name: "render_range", description: "Render a range and return the audio path", area: "audio", inputSchema: {} },
  { name: "plugin_search", description: "Search the plugin catalog", area: "plugins", inputSchema: {} },
  { name: "transport_play", description: "Start playback", area: "transport", inputSchema: {} },
];

const BIG_SET = {
  id: "big",
  title: "Big set",
  description: "Eight tools, one of them undescribed",
  tools: ["big_1", "big_2", "big_3", "big_4", "big_5", "big_6", "big_7", "big_8"],
  default_on: false,
};

function mockBackend(options: { sets?: unknown[]; tools?: unknown[] } = {}) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") return { version: 2, active_provider: "", providers: [] };
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return { version: "test", providers: [] };
    if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    if (cmd === "ai_toolsets") return options.sets ?? [];
    if (cmd === "ai_tools") return options.tools ?? [];
    return makeSnapshot();
  });
}

async function mount(options?: { sets?: unknown[]; tools?: unknown[] }) {
  mockBackend(options);
  await store.initProject();
  const view = render(() => <><AiPanel /><PreferencesHost /><TooltipLayer /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  // The toolset and tool catalogs load after the config; the last read marks
  // the state the chips render from.
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_tools")).toBe(true));
  return view;
}

function chips(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".ai-toolset-chip")].map((chip) => chip.textContent ?? "");
}

function settingsDialog(): HTMLElement {
  return screen.getByRole("dialog", { name: /AI settings|Preferences/ });
}

async function openSettings() {
  fireEvent.click(screen.getByRole("button", { name: "Provider settings" }));
  await waitFor(() => expect(settingsDialog()).toBeTruthy());
}

function toolsetRow(title: string): HTMLElement {
  const row = [...document.querySelectorAll(".ai-toolset-row")]
    .find((element) => element.textContent?.includes(title));
  if (!row) throw new Error(`no toolset row for ${title}`);
  return row as HTMLElement;
}

beforeEach(() => invokeMock.mockReset());
afterEach(cleanup);

describe("toolset thumbnail", () => {
  it("shows one chip per enabled set and opens the settings from the row", async () => {
    const view = await mount({ sets: SETS, tools: TOOLS });
    expect(chips(view.container)).toEqual(["dsl-edit", "project-edit", "listen", "plugins"]);
    expect(view.container.querySelector(".ai-toolset-chip-more")).toBeNull();

    fireEvent.click(view.container.querySelector(".ai-toolsets-strip") as HTMLElement);
    await waitFor(() => expect(settingsDialog()).toBeTruthy());
  });

  it("shows the whole surface as one all chip when the backend has no toolsets", async () => {
    const view = await mount({ sets: [], tools: TOOLS });
    expect(chips(view.container)).toEqual(["all"]);
    fireEvent.focusIn(view.container.querySelector(".ai-toolset-chip") as HTMLElement);
    expect((await screen.findByRole("tooltip", {}, { timeout: 3000 })).textContent).toContain("every panel tool is offered");
  });

  it("shows the empty selection as a warning chip that explains the prose-only mode", async () => {
    const view = await mount({ sets: [{ ...SETS[0], default_on: false }], tools: TOOLS });
    expect(chips(view.container)).toEqual(["no tools"]);
    const chip = view.container.querySelector(".ai-toolset-chip") as HTMLElement;
    expect(chip.classList.contains("ai-toolset-chip-empty")).toBe(true);
    fireEvent.focusIn(chip);
    expect((await screen.findByRole("tooltip", {}, { timeout: 3000 })).textContent).toContain("answer in prose");
  });

  it("updates the chips immediately when the modal toggles a set and collapses extras into +N", async () => {
    const view = await mount({ sets: SETS, tools: TOOLS });
    await openSettings();

    fireEvent.click(screen.getByRole("checkbox", { name: /Transport/ }));
    expect(chips(view.container)).toEqual(["dsl-edit", "project-edit", "listen", "plugins", "+1"]);

    const more = view.container.querySelector(".ai-toolset-chip-more") as HTMLElement;
    fireEvent.focusIn(more);
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 3000 });
    expect(tooltip.textContent).toContain("1 more toolsets");
    expect(tooltip.textContent).toContain("transport");
    expect(tooltip.textContent).toContain("Move and query the playhead");
  });

  it("lists the hidden sets when several extras are enabled", async () => {
    const view = await mount({ sets: [...SETS.slice(0, 4), { ...SETS[4], default_on: true }, { ...BIG_SET, default_on: true }], tools: TOOLS });
    expect(chips(view.container)).toEqual(["dsl-edit", "project-edit", "listen", "plugins", "+2"]);
    fireEvent.focusIn(view.container.querySelector(".ai-toolset-chip-more") as HTMLElement);
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 3000 });
    expect(tooltip.textContent).toContain("2 more toolsets");
    expect(tooltip.textContent).toContain("transport");
    expect(tooltip.textContent).toContain("Eight tools, one of them undescribed");
  });
});

describe("toolset hover in the settings modal", () => {
  it("lists the set's tools with their own descriptions, falling back when one is missing", async () => {
    await mount({ sets: SETS, tools: TOOLS });
    await openSettings();

    fireEvent.mouseEnter(toolsetRow("DSL edit"));
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 3000 });
    expect(tooltip.textContent).toContain("Read a subtree and propose one whole-subtree replacement");
    expect(tooltip.textContent).toContain("edit_subtree");
    expect(tooltip.textContent).toContain("Propose one complete subtree replacement");
    // project_get has no description in the payload.
    expect(tooltip.textContent).toContain("no description");
  });

  it("truncates long tool lists with the remaining count", async () => {
    await mount({
      sets: [BIG_SET],
      tools: BIG_SET.tools.map((name) => ({ name, description: `${name} does a thing`, area: "test", inputSchema: {} })),
    });
    await openSettings();

    const row = toolsetRow("Big set");
    expect(row.textContent).toContain("8 tools");
    fireEvent.mouseEnter(row);
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 3000 });
    expect(tooltip.textContent).toContain("big_6 does a thing");
    expect(tooltip.textContent).not.toContain("big_7 does a thing");
    expect(tooltip.textContent).toContain("… and 2 more");
  });
});

describe("dialog escape ownership", () => {
  it("lets only the topmost dialog consume Escape", async () => {
    await mount({ sets: SETS, tools: TOOLS });
    await openSettings();
    // Add provider opens from inside settings; the stack must hand Escape to
    // whichever dialog opened last and return focus to the button it came from.
    const addProvider = screen.getByRole("button", { name: "Add provider" });
    addProvider.focus();
    fireEvent.click(addProvider);
    await waitFor(() => expect(screen.getByRole("dialog", { name: "New provider" })).toBeTruthy());

    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New provider" })).toBeNull());
    expect(screen.getByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add provider" }));

    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
  });
});

describe("toolset tooltip data", () => {
  it("builds the tool description map from one catalog read, not per hover", async () => {
    await mount({ sets: SETS, tools: TOOLS });
    await openSettings();
    fireEvent.mouseEnter(toolsetRow("Listen"));
    await screen.findByRole("tooltip", {}, { timeout: 3000 });
    fireEvent.mouseLeave(toolsetRow("Listen"));

    const reads = invokeMock.mock.calls.filter(([command]) => command === "ai_tools");
    expect(reads.length).toBe(1);
    expect(reads[0][1]).toEqual({});
  });
});
