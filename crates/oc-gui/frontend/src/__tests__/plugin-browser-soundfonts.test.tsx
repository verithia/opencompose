/**
 * Instrument packs in the plugin browser: the plugin list carries a labelled
 * Instruments group whose rows show size, license and install state, stream a
 * download progress line, and surface a refused install verbatim.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { PluginBrowser } from "../components/PluginBrowser";
import { i18n } from "../stores/i18n";
import type { SoundfontItem, SoundfontListing } from "../stores/api";

// The browser listens on more than one channel, so handlers are keyed by name.
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

const PACK: SoundfontItem = {
  id: "ms-basic",
  name: "MS Basic",
  version: "1.0.0",
  description: "General MIDI sound set.",
  bytes: 50_003_968,
  license: {
    name: "MIT",
    url: "https://example.com/MS%20Basic_License.md",
    text: "MuseScore_General is MIT; derived from FluidR3Mono and FluidR3.",
  },
  installed: false,
  installed_version: null,
  installed_bytes: null,
  path: null,
  source: "index",
};

function listing(installed: boolean): SoundfontListing {
  return {
    dir: "/Users/test/.opencompose/soundfonts",
    items: [
      {
        ...PACK,
        installed,
        installed_version: installed ? PACK.version : null,
        installed_bytes: installed ? PACK.bytes : null,
        path: installed ? `${"/Users/test/.opencompose/soundfonts"}/ms-basic.sf3` : null,
      },
    ],
  };
}

interface Backend {
  installed: boolean;
  installError?: string;
  /** Holds the install request open so the progress line can be observed. */
  pending?: boolean;
  release?: () => void;
}

function mockBackend(backend: Backend) {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "soundfonts_list") return listing(backend.installed);
    if (command === "scan_plugins") return [];
    if (command === "list_sampler_assets") return [];
    if (command === "project_compatibility_report") {
      return {
        compatible: true,
        installed_plugin_count: 0,
        resolved_plugins: [],
        issues: [],
      };
    }
    if (command === "soundfont_install") {
      if (backend.installError) throw backend.installError;
      if (backend.pending) {
        await new Promise<void>((resolve) => {
          backend.release = resolve;
        });
      }
      backend.installed = true;
      return { ...PACK, sha256: "0".repeat(64), installed_at: 1 };
    }
    if (command === "soundfonts_remove") {
      backend.installed = false;
      return { ...PACK, sha256: "0".repeat(64), installed_at: 1 };
    }
    return makeSnapshot();
  });
}

beforeEach(() => {
  i18n.setLocale("en");
  events.handlers.clear();
});

afterEach(() => {
  cleanup();
  invokeMock.mockReset();
  i18n.setLocale("en");
});

describe("plugin browser instrument packs", () => {
  it("lists an index pack with its size, license and attribution one click away", async () => {
    mockBackend({ installed: false });
    render(() => <PluginBrowser />);

    await screen.findByText("MS Basic");
    // The group header sits above one 24 px row for the pack.
    expect(screen.getByText("1 downloadable")).toBeInTheDocument();
    expect(document.querySelector('[data-soundfont-id="ms-basic"]')).not.toBeNull();
    expect(screen.getByText("47.7 MB")).toBeInTheDocument();
    expect(screen.queryByText(PACK.license.text)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "MIT" }));

    await waitFor(() => expect(screen.getByText(PACK.license.text)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Install" })).toBeInTheDocument();
    expect(screen.queryByText("INSTALLED")).toBeNull();
  });

  it("streams download progress and then reports the installed state", async () => {
    const backend: Backend = { installed: false, pending: true };
    mockBackend(backend);
    render(() => <PluginBrowser />);

    await screen.findByText("MS Basic");
    fireEvent.click(screen.getByRole("button", { name: "Install" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("soundfont_install", { id: "ms-basic" }));
    expect(screen.getByText("Installing…")).toBeInTheDocument();

    events.handlers.get("soundfont_progress")?.({
      payload: { id: "ms-basic", received: 12_845_056, total: 50_003_968 },
    });

    await waitFor(() => expect(screen.getByText("12.3 / 47.7 MB")).toBeInTheDocument());

    backend.release?.();
    await waitFor(() => expect(screen.getByText("INSTALLED")).toBeInTheDocument());
    const remove = screen.getByRole("button", { name: "Remove" });
    expect(remove).toBeInTheDocument();

    fireEvent.click(remove);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("soundfonts_remove", { id: "ms-basic" }));
    await waitFor(() => expect(screen.queryByText("INSTALLED")).toBeNull());
  });

  it("shows a refused install with the backend's reason", async () => {
    mockBackend({
      installed: false,
      installError:
        "Instrument pack 'ms-basic' failed sha256 verification; nothing was installed",
    });
    render(() => <PluginBrowser />);

    await screen.findByText("MS Basic");
    fireEvent.click(screen.getByRole("button", { name: "Install" }));

    await waitFor(() =>
      expect(
        screen.getByText(
          "Install failed: Instrument pack 'ms-basic' failed sha256 verification; nothing was installed",
        ),
      ).toBeInTheDocument(),
    );
    expect(screen.getByRole("alert")).toHaveTextContent("failed sha256 verification");
    expect(screen.queryByText("INSTALLED")).toBeNull();
  });
});
