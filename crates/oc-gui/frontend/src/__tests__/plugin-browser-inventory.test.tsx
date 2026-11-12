/**
 * Inventory refresh in the plugin browser: the Rescan button walks the
 * bundles, while a background refresh that lands after the first paint
 * repaints the list without a spinner.
 */
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { PluginBrowser } from "../components/PluginBrowser";
import { i18n } from "../stores/i18n";

const events = vi.hoisted(() => ({
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
    events.handlers.set(name, handler);
    return () => {
      events.handlers.delete(name);
    };
  },
}));

function pluginInfo(name: string) {
  return {
    id: `vst3:${name.toLowerCase()}`,
    name,
    vendor: "Vendor",
    format: "Vst3",
    category: "Instrument",
    path: `/plugins/${name}.vst3`,
    num_params: 0,
  };
}

/** Answers each scan with the next inventory in the list. */
function backend(inventories: string[][]) {
  let scan = 0;
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "scan_plugins") {
      const names = inventories[Math.min(scan, inventories.length - 1)];
      scan += 1;
      return names.map(pluginInfo);
    }
    if (command === "soundfonts_list") return { dir: "/fonts", items: [] };
    if (command === "list_sampler_assets") return [];
    if (command === "project_compatibility_report") {
      return { compatible: true, installed_plugin_count: 0, resolved_plugins: [], issues: [] };
    }
    return makeSnapshot();
  });
}

const scanCalls = () =>
  invokeMock.mock.calls.filter(([command]) => command === "scan_plugins");

beforeEach(() => {
  i18n.setLocale("en");
  events.handlers.clear();
  invokeMock.mockClear();
});

afterEach(() => cleanup());

it("scans again only when the Rescan button is pressed", async () => {
  backend([["First Plugin"], ["First Plugin", "Installed Later"]]);
  render(() => <PluginBrowser />);

  await screen.findByText("First Plugin");
  expect(scanCalls()[0]).toEqual(["scan_plugins", { rescan: false }]);

  fireEvent.click(screen.getByRole("button", { name: "Rescan" }));

  await screen.findByText("Installed Later");
  expect(scanCalls()[1]).toEqual(["scan_plugins", { rescan: true }]);
});

it("repaints the list when a background inventory refresh lands", async () => {
  backend([["First Plugin"], ["Installed Later"]]);
  render(() => <PluginBrowser />);

  await screen.findByText("First Plugin");
  expect(screen.queryByRole("button", { name: "Scanning…" })).toBeNull();

  events.handlers.get("plugin_inventory_updated")?.({ payload: 2 });

  await screen.findByText("Installed Later");
  expect(scanCalls()[1]).toEqual(["scan_plugins", { rescan: false }]);
  // The quiet refresh never puts the button into its scanning state.
  expect(screen.queryByRole("button", { name: "Scanning…" })).toBeNull();
  await waitFor(() => expect(screen.queryByText("First Plugin")).toBeNull());
});
