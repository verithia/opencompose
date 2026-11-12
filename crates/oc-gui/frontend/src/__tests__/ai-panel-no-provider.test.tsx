/**
 * An install with no saved provider: the catalogue is a template list for Add
 * provider, never rows in the user's configuration. The panel must say so,
 * block sending, and write nothing until the user creates one.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { aiConfig, getActiveProvider, hasProvider, loadConfig } from "../ai/config";

function mockBackend() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") {
      return { version: 2, active_provider: "", providers: [] };
    }
    if (cmd === "save_ai_config") return { path: "", warnings: [] };
    if (cmd === "ai_key_status") return false;
    if (cmd === "ai_catalog") return { version: "test", providers: [] };
    if (cmd === "ai_model_limits") {
      return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    }
    return makeSnapshot();
  });
}

function calls(name: string) {
  return invokeMock.mock.calls.filter(([command]) => command === name);
}

beforeEach(() => {
  invokeMock.mockReset();
  mockBackend();
});

afterEach(cleanup);

describe("AI panel without a configured provider", () => {
  it("loads no catalogue rows and reports no provider", async () => {
    await loadConfig();
    expect(hasProvider()).toBe(false);
    expect(aiConfig.config.providers).toEqual([]);
    // The catalogue is not the user's file: nothing is written on first run.
    expect(calls("save_ai_config").length).toBe(0);
    // Reading the active provider stays total without inventing a catalogue row.
    expect(getActiveProvider().id).toBe("");
    expect(getActiveProvider().model).toBe("");
  });

  it("shows one empty state instead of a provider count", async () => {
    await store.initProject();
    const view = render(() => <AiPanel />);
    await waitFor(() => {
      expect(view.container.querySelector(".ai-provider-summary-empty")).toBeTruthy();
    });
    const summary = view.container.querySelector(".ai-provider-summary")!;
    expect(summary.textContent).toBe("No provider configured — add one to send messages.");
    expect(summary.textContent).not.toMatch(/\d+ providers/);
  });

  it("blocks sending with the provider reason, not a model reason", async () => {
    await store.initProject();
    const view = render(() => <AiPanel />);
    await waitFor(() => expect(view.container.querySelector(".ai-provider-summary-empty")).toBeTruthy());
    const composer = view.container.querySelector("textarea")!;
    fireEvent.input(composer, { target: { value: "hello" } });
    await waitFor(() => {
      expect(view.container.querySelector(".ai-send-blocked")?.textContent)
        .toBe("Add a provider before sending.");
    });
    expect(screen.queryByText("Pick a model before sending.")).toBeNull();
  });

  it("offers one Add button in the settings page instead of an inert form", async () => {
    await store.initProject();
    const view = render(() => <><AiPanel /><PreferencesHost /></>);
    await waitFor(() => expect(view.container.querySelector(".ai-provider-summary-empty")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Provider settings" }));
    await waitFor(() => expect(screen.getByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy());
    const empty = document.querySelector(".ai-settings-empty")!;
    expect(empty.textContent).toContain("No provider configured");
    expect(within(empty as HTMLElement).getByRole("button", { name: "Add provider" })).toBeTruthy();
    // The connection fields are not rendered against a provider that does not exist.
    expect(document.querySelector(".ai-settings-provider")).toBeNull();
  });
});
