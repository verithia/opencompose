/**
 * A panel's close button closes the panel it is in: the floating window when the
 * panel floats, the sidebar when it is docked. It never closes the surface the
 * panel is not in.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "../setup";
import App from "../../App";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";

function mockBackend() {
  invokeMock.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "load_ai_config": return { version: 2, active_provider: "", providers: [] };
      case "ai_key_status": return true;
      case "ai_catalog": return { version: "test", providers: [] };
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "sessions_list": return [];
      case "workspaces_list": return [];
      case "workspace_current": return { path: "/songs", name: "songs", detached: false, projects: [] };
      case "scan_plugins": return [];
      case "plugin_catalog": return { platform: "macos-aarch64", entries: [], installed: [] };
      default: return makeSnapshot();
    }
  });
}

/** The AI header's close control inside one surface. */
function closeButton(container: HTMLElement, surface: string) {
  return container.querySelector(`.${surface} [aria-label="${i18n.t("ai.header.close")}"]`);
}

beforeEach(() => {
  i18n.setLocale("en");
  mockBackend();
  for (const panel of [...panelStore.floatingPanels]) panelStore.dockPanel(panel.id);
  panelStore.closeSidebar();
  panelStore.closeBottom();
});

afterEach(() => {
  for (const panel of [...panelStore.floatingPanels]) panelStore.dockPanel(panel.id);
  panelStore.closeSidebar();
  panelStore.closeBottom();
  i18n.setLocale("en");
});

describe("closing a panel", () => {
  it("closes the floating AI panel and leaves the sidebar alone", async () => {
    const view = render(() => <App />);
    panelStore.openSidebar("dsl");
    panelStore.detachPanel("ai");
    await waitFor(() => expect(view.container.querySelector(".floating-panel .ai-panel")).toBeTruthy());

    fireEvent.click(closeButton(view.container, "floating-panel")!);

    await waitFor(() => expect(view.container.querySelector(".floating-panel")).toBeNull());
    expect(panelStore.floatingPanels).toHaveLength(0);
    expect(panelStore.sidebarOpen()).toBe(true);
    expect(panelStore.sidebarTab()).toBe("dsl");
  });

  it("closes the sidebar from the docked AI panel", async () => {
    const view = render(() => <App />);
    panelStore.openSidebar("ai");
    await waitFor(() => expect(view.container.querySelector(".sidebar-content .ai-panel")).toBeTruthy());

    fireEvent.click(closeButton(view.container, "sidebar-content")!);

    await waitFor(() => expect(panelStore.sidebarOpen()).toBe(false));
    expect(panelStore.floatingPanels).toHaveLength(0);
  });
});
