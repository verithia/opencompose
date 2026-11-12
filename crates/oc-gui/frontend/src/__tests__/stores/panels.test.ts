/**
 * Panel store tests — split-view layout, bottom panels, sidebar, floating.
 */
import { describe, it, expect } from "vitest";
import { panelStore } from "../../stores/panels";

describe("panelStore", () => {
  describe("initial state", () => {
    it("bottom panel starts closed", () => {
      expect(panelStore.bottomPanel()).toBeNull();
    });

    it("sidebar starts closed", () => {
      expect(panelStore.sidebarOpen()).toBe(false);
    });

    it("no floating panels initially", () => {
      expect(panelStore.floatingPanels.length).toBe(0);
    });

    it("split ratio defaults to 0.55", () => {
      expect(panelStore.splitRatio()).toBe(0.55);
    });
  });

  describe("bottom panel", () => {
    it("opens a bottom panel", () => {
      panelStore.openBottom("pianoroll");
      expect(panelStore.bottomPanel()).toBe("pianoroll");
      panelStore.closeBottom();
    });

    it("switches between bottom panels", () => {
      panelStore.openBottom("pianoroll");
      expect(panelStore.bottomPanel()).toBe("pianoroll");
      panelStore.openBottom("mixer");
      expect(panelStore.bottomPanel()).toBe("mixer");
      panelStore.closeBottom();
    });

    it("closes bottom panel", () => {
      panelStore.openBottom("mixer");
      panelStore.closeBottom();
      expect(panelStore.bottomPanel()).toBeNull();
    });

    it("toggleBottom opens when closed", () => {
      panelStore.closeBottom();
      panelStore.toggleBottom("pianoroll");
      expect(panelStore.bottomPanel()).toBe("pianoroll");
      panelStore.closeBottom();
    });

    it("toggleBottom closes when same panel is active", () => {
      panelStore.openBottom("pianoroll");
      panelStore.toggleBottom("pianoroll");
      expect(panelStore.bottomPanel()).toBeNull();
    });

    it("toggleBottom switches when different panel is active", () => {
      panelStore.openBottom("pianoroll");
      panelStore.toggleBottom("mixer");
      expect(panelStore.bottomPanel()).toBe("mixer");
      panelStore.closeBottom();
    });

    it("allows custom split ratio", () => {
      panelStore.openBottom("pianoroll", 0.4);
      expect(panelStore.splitRatio()).toBe(0.4);
      panelStore.closeBottom();
    });
  });

  describe("split divider", () => {
    it("clamps ratio to valid range", () => {
      panelStore.updateSplitRatio(0.1);
      expect(panelStore.splitRatio()).toBe(0.2); // min clamp
      panelStore.updateSplitRatio(0.95);
      expect(panelStore.splitRatio()).toBe(0.85); // max clamp
      panelStore.updateSplitRatio(0.55); // reset
    });
  });

  describe("right sidebar", () => {
    it("opens sidebar with default tab", () => {
      panelStore.openSidebar();
      expect(panelStore.sidebarOpen()).toBe(true);
      panelStore.closeSidebar();
    });

    it("opens sidebar with specific tab", () => {
      panelStore.openSidebar("dsl");
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("dsl");
      panelStore.closeSidebar();
    });

    it("selectSidebarTab opens sidebar if closed", () => {
      panelStore.closeSidebar();
      panelStore.selectSidebarTab("plugins");
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("plugins");
      panelStore.closeSidebar();
    });

    it("toggleSidebar closes when same tab is active", () => {
      panelStore.openSidebar("dsl");
      panelStore.toggleSidebar("dsl");
      expect(panelStore.sidebarOpen()).toBe(false);
    });

    it("toggleSidebar switches tab when different", () => {
      panelStore.openSidebar("dsl");
      panelStore.toggleSidebar("plugins");
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("plugins");
      panelStore.closeSidebar();
    });
  });

  describe("floating panels", () => {
    it("detaches a panel to floating", () => {
      panelStore.detachPanel("pianoroll");
      expect(panelStore.isFloating("pianoroll")).toBe(true);
      const fp = panelStore.getFloating("pianoroll");
      expect(fp).toBeDefined();
      expect(fp!.width).toBeGreaterThan(0);
      panelStore.dockPanel("pianoroll");
    });

    it("docking a panel removes it from floating", () => {
      panelStore.detachPanel("mixer");
      panelStore.dockPanel("mixer");
      expect(panelStore.isFloating("mixer")).toBe(false);
    });

    it("docking a bottom panel re-opens it in bottom", () => {
      panelStore.detachPanel("pianoroll");
      panelStore.dockPanel("pianoroll");
      expect(panelStore.bottomPanel()).toBe("pianoroll");
      panelStore.closeBottom();
    });

    it("docking a sidebar panel selects it in sidebar", () => {
      panelStore.detachPanel("dsl");
      panelStore.dockPanel("dsl");
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("dsl");
      panelStore.closeSidebar();
    });

    it("does not double-detach", () => {
      panelStore.detachPanel("mixer");
      const count = panelStore.floatingPanels.length;
      panelStore.detachPanel("mixer");
      expect(panelStore.floatingPanels.length).toBe(count);
      panelStore.dockPanel("mixer");
    });

    it("detaching active bottom panel closes bottom", () => {
      panelStore.openBottom("pianoroll");
      panelStore.detachPanel("pianoroll");
      expect(panelStore.bottomPanel()).toBeNull();
      panelStore.dockPanel("pianoroll");
      panelStore.closeBottom();
    });

    it("movePanel updates position", () => {
      panelStore.detachPanel("mixer");
      panelStore.movePanel("mixer", 200, 300);
      const fp = panelStore.getFloating("mixer");
      expect(fp?.x).toBe(200);
      expect(fp?.y).toBe(300);
      panelStore.dockPanel("mixer");
    });

    it("resizePanel enforces minimums", () => {
      panelStore.detachPanel("mixer");
      panelStore.resizePanel("mixer", 100, 50);
      const fp = panelStore.getFloating("mixer");
      expect(fp?.width).toBe(200); // min 200
      expect(fp?.height).toBe(100); // min 100
      panelStore.dockPanel("mixer");
    });

    it("bringToFront increases z-index", () => {
      panelStore.detachPanel("mixer");
      const before = panelStore.getFloating("mixer")?.zIndex ?? 0;
      panelStore.bringToFront("mixer");
      const after = panelStore.getFloating("mixer")?.zIndex ?? 0;
      expect(after).toBeGreaterThan(before);
      panelStore.dockPanel("mixer");
    });
  });

  describe("closing a panel", () => {
    it("removes a floating panel without closing the sidebar", () => {
      panelStore.openSidebar("dsl");
      panelStore.detachPanel("ai");
      panelStore.closePanel("ai");
      expect(panelStore.isFloating("ai")).toBe(false);
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("dsl");
      panelStore.closeSidebar();
    });

    it("closes the docked sidebar from a sidebar panel", () => {
      panelStore.openSidebar("ai");
      panelStore.closePanel("ai");
      expect(panelStore.sidebarOpen()).toBe(false);
    });

    it("closes the bottom panel from a bottom panel", () => {
      panelStore.openBottom("clips");
      panelStore.closePanel("clips");
      expect(panelStore.bottomPanel()).toBeNull();
    });
  });

  describe("legacy compat", () => {
    it("toggleAi opens sidebar with ai tab", () => {
      panelStore.closeSidebar();
      panelStore.toggleAi();
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("ai");
      panelStore.toggleAi(); // close
    });

    it("selectDockedTab routes bottom panels correctly", () => {
      panelStore.selectDockedTab("mixer");
      expect(panelStore.bottomPanel()).toBe("mixer");
      panelStore.closeBottom();
    });

    it("selectDockedTab routes sidebar panels correctly", () => {
      panelStore.selectDockedTab("dsl");
      expect(panelStore.sidebarOpen()).toBe(true);
      expect(panelStore.sidebarTab()).toBe("dsl");
      panelStore.closeSidebar();
    });

    it("getPanel returns docked shim for non-floating panels", () => {
      const p = panelStore.getPanel("pianoroll");
      expect(p?.mode).toBe("docked");
    });

    it("getPanel returns floating for detached panels", () => {
      panelStore.detachPanel("pianoroll");
      const p = panelStore.getPanel("pianoroll");
      expect(p?.mode).toBe("floating");
      panelStore.dockPanel("pianoroll");
      panelStore.closeBottom();
    });
  });
});
