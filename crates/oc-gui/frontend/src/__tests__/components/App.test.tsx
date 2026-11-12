/**
 * Component smoke tests — verify the split-view layout renders.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "../setup";
import App from "../../App";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";

beforeEach(() => {
  vi.clearAllMocks();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "take_startup_project") return null;
    if (cmd === "scan_plugins") return [];
    return makeSnapshot();
  });
});

describe("App", () => {
  it("renders the toolbar", async () => {
    const { container } = render(() => <App />);
    const toolbar = container.querySelector(".daw-toolbar");
    expect(toolbar).toBeInTheDocument();
    // Primary transport controls plus the compact secondary-settings affordance.
    const transportBtns = container.querySelectorAll(".transport-btn");
    // stop + play + loop + record + click + settings (write automation is in the popover).
    expect(transportBtns.length).toBe(6);
    expect(container.querySelector("[aria-label='Transport settings']")).toBeInTheDocument();
    const loop = container.querySelector<HTMLButtonElement>(".transport-btn.loop");
    expect(loop?.getAttribute("aria-label")).toBe("Loop");
  });

  it("renders Timeline in the top split (always visible)", async () => {
    const { container } = render(() => <App />);
    // Timeline is always rendered inside the split-top area
    const splitTop = container.querySelector(".split-top");
    expect(splitTop).toBeInTheDocument();
    const timelinePanel = container.querySelector(".timeline-panel");
    expect(timelinePanel).toBeInTheDocument();
  });

  it("renders sidebar toggle when sidebar is closed", async () => {
    const { container } = render(() => <App />);
    const toggle = container.querySelector(".sidebar-toggle-strip");
    expect(toggle).toBeInTheDocument();
  });

  it("shows status bar with track count", async () => {
    const { findByText } = render(() => <App />);
    expect(await findByText("0 tracks")).toBeInTheDocument();
  });

  it("keeps status information but no longer renders the project path", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
    if (cmd === "scan_plugins") return [];
      return { ...makeSnapshot(), file_path: "/tmp/hidden/project.ocp" };
    });
    const { container, findByText } = render(() => <App />);
    expect(await findByText("0 tracks")).toBeInTheDocument();
    expect(container.querySelector(".status-bar")).toBeTruthy();
    expect(container.querySelector(".status-path")).toBeFalsy();
    expect(container.textContent).not.toContain("/tmp/hidden/project.ocp");
  });
  it("renders a line icon for every sidebar tab", () => {
    panelStore.openSidebar("dsl");
    try {
      const { container } = render(() => <App />);
      const tabs = [...container.querySelectorAll(".sidebar-tab")];
      expect(tabs).toHaveLength(4);
      // Emoji and bare text are gone: each tab is exactly one decorative svg.
      for (const tab of tabs) {
        expect(tab.querySelector("svg")).toBeTruthy();
        expect(tab.textContent?.trim()).toBe("");
      }
    } finally {
      panelStore.closeSidebar();
    }
  });

  it("puts the sidebar tabs in one row above the panel and names the open one", () => {
    panelStore.openSidebar("plugins");
    try {
      const { container } = render(() => <App />);
      const sidebar = container.querySelector(".right-sidebar")!;
      const topbar = sidebar.querySelector(".sidebar-topbar")!;
      // The strip is the sidebar's first child, above the panel content: the
      // full-height column is gone.
      expect(sidebar.firstElementChild).toBe(topbar);
      expect(topbar.querySelectorAll(".sidebar-tabs")).toHaveLength(1);
      const tabs = [...topbar.querySelectorAll(".sidebar-tab")];
      expect(tabs).toHaveLength(4);
      // One row: every tab is a child of the same strip, and the open panel is
      // named after them.
      expect(tabs.map((tab) => tab.getAttribute("aria-label"))).toEqual([
        i18n.t("panel.dsl"),
        i18n.t("panel.plugins"),
        i18n.t("panel.store"),
        i18n.t("panel.ai"),
      ]);
      expect(topbar.querySelector(".sidebar-tab-name")!.textContent).toBe(i18n.t("panel.plugins"));
      const content = sidebar.querySelector(".sidebar-content")!;
      expect(topbar.compareDocumentPosition(content) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    } finally {
      panelStore.closeSidebar();
    }
  });

  it("names the panel the row switches to", () => {
    panelStore.openSidebar("plugins");
    try {
      const { container } = render(() => <App />);
      const name = () => container.querySelector(".sidebar-tab-name")!.textContent;
      const storeTab = [...container.querySelectorAll(".sidebar-tab")]
        .find((tab) => tab.getAttribute("aria-label") === i18n.t("panel.store"))!;
      fireEvent.click(storeTab);
      expect(name()).toBe(i18n.t("panel.store"));
      expect(storeTab.classList.contains("active")).toBe(true);
      // The row still carries one name, not one per tab.
      expect(container.querySelectorAll(".sidebar-tab-name")).toHaveLength(1);
    } finally {
      panelStore.closeSidebar();
    }
  });

  it("keeps the collapse control at the end of the tab row", () => {
    panelStore.openSidebar("plugins");
    try {
      const { container } = render(() => <App />);
      const collapse = container.querySelector(".sidebar-topbar .sidebar-collapse")!;
      expect(collapse).toBeTruthy();
      expect(collapse.getAttribute("aria-label")).toBe(i18n.t("panel.collapseSidebar"));
      fireEvent.click(collapse);
      expect(panelStore.sidebarOpen()).toBe(false);
      expect(container.querySelector(".right-sidebar")).toBeNull();
      // The collapsed strip is still the way back.
      const strip = container.querySelector(".sidebar-toggle-strip")!;
      expect(strip).toBeTruthy();
      fireEvent.click(strip);
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(container.querySelector(".right-sidebar")).toBeTruthy();
    } finally {
      panelStore.closeSidebar();
    }
  });

  it("resizes immediately from the CSS-constrained visible sidebar width", () => {    panelStore.setSidebarWidth(320);
    panelStore.openSidebar("dsl");
    try {
      const { container } = render(() => <App />);
      const sidebar = container.querySelector(".right-sidebar")!;
      vi.spyOn(sidebar, "getBoundingClientRect").mockReturnValue({ width: 440 } as DOMRect);
      fireEvent.pointerDown(container.querySelector(".sidebar-resize-handle")!, { button: 0, pointerId: 1, clientX: 600 });
      fireEvent.pointerMove(document, { pointerId: 1, clientX: 590 });
      fireEvent.pointerUp(window, { pointerId: 1 });
      expect(panelStore.sidebarWidth()).toBe(450);
    } finally {
      panelStore.closeSidebar();
      panelStore.setSidebarWidth(320);
    }
  });

});

describe("project rename", () => {
  it("renames the project from the status bar", async () => {
    const { container } = render(() => <App />);
    const name = await waitFor(() => {
      const element = container.querySelector(".status-project");
      expect(element).toBeTruthy();
      return element as HTMLElement;
    });
    fireEvent.dblClick(name);
    const input = document.querySelector<HTMLInputElement>(".text-prompt input")!;
    expect(input).toBeTruthy();
    fireEvent.input(input, { target: { value: "Night Drive" } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_project_name")).toBe(true);
    });
  });
});
