import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { WindowControls } from "../../components/WindowControls";

const api = vi.hoisted(() => ({
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn(),
  isMaximized: vi.fn(async () => false),
  onResized: vi.fn(async () => () => {}),
  startResizeDragging: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => api,
}));

const original = navigator.userAgent;
function windows(on: boolean) {
  Object.defineProperty(navigator, "userAgent", {
    value: on ? "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" : "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
    configurable: true,
  });
}

beforeEach(() => {
  windows(true);
  for (const fn of Object.values(api)) {
    if (typeof fn === "function" && "mockClear" in fn) (fn as { mockClear: () => void }).mockClear();
  }
  (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
});

describe("WindowControls", () => {
  it("draws caption buttons on Windows and wires them to the window", async () => {
    const { container } = render(() => <WindowControls />);
    const buttons = container.querySelectorAll(".window-control");
    expect(buttons).toHaveLength(3);

    fireEvent.click(container.querySelector('[aria-label="Minimize"]')!);
    fireEvent.click(container.querySelector('[aria-label="Maximize"]')!);
    fireEvent.click(container.querySelector('[aria-label="Close"]')!);
    await vi.waitFor(() => expect(api.close).toHaveBeenCalled());
    expect(api.minimize).toHaveBeenCalled();
    expect(api.toggleMaximize).toHaveBeenCalled();
    windows(false);
  });

  it("stays out of the way on macOS, which keeps its traffic lights", () => {
    windows(false);
    const { container } = render(() => <WindowControls />);
    expect(container.querySelector(".window-controls")).toBeFalsy();
    windows(true);
  });

  it("stays hidden outside Tauri", () => {
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    const { container } = render(() => <WindowControls />);
    expect(container.querySelector(".window-controls")).toBeFalsy();
    expect(original.length).toBeGreaterThan(0);
  });
});
