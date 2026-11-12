/**
 * AI settings surface: the whole settings block lives in a portalled modal, so
 * the narrow sidebar keeps only the header's model/effort selectors, a summary,
 * the toolset thumbnail and the buttons that open this dialog. The dialog lists
 * the providers itself, since the column edits the active one.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { getActiveProvider } from "../ai/config";

function mockBackend() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") return aiConfigWire();
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return { version: "test", providers: [] };
    if (cmd === "ai_model_limits") {
      return {
        provider_id: "deepseek-v4-flash",
        model_id: "deepseek-flash",
        bundled: { context: 1000000, max_output: 8192, input: { text: true, image: false, video: false, audio: false }, price: null },
        overridden: null,
      };
    }
    return makeSnapshot();
  });
}

async function mount() {
  await store.initProject();
  const view = render(() => <><AiPanel /><PreferencesHost /></>);
  // The panel installs defaults and saves them once before sending is allowed.
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  return view;
}

/** The header toggle: its accessible name is stable, the label does not flip. */
function settingsButton(): HTMLElement {
  return screen.getByRole("button", { name: "Provider settings" });
}

const dialog = () => screen.queryByRole("dialog", { name: /AI settings|Preferences/ });

beforeEach(async () => {
  invokeMock.mockReset();
  mockBackend();
});

afterEach(cleanup);

describe("AI panel settings modal", () => {
  it("opens from the Settings button with the limits entry and closes with Done", async () => {
    await mount();
    expect(dialog()).toBeNull();

    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(screen.getByRole("button", { name: "Edit limits" })).toBeTruthy();
    expect(screen.getByText("Context (tokens)")).toBeTruthy();
    // The provider action lives in the dialog's connection column.
    expect(dialog()!.contains(screen.getByRole("button", { name: "Add provider" }))).toBe(true);
    // Focus starts in the first field, not on the header's close button.
    expect(dialog()!.contains(document.activeElement)).toBe(true);
    expect(document.activeElement?.tagName).toBe("SELECT");

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());
    expect(screen.queryByRole("button", { name: "Edit limits" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Add provider" })).toBeNull();
    expect(screen.queryByText(/context budget/i)).toBeNull();
  });

  it("keeps Add provider in the dialog and out of the closed panel", async () => {
    const view = await mount();
    expect(screen.queryByRole("button", { name: "Add provider" })).toBeNull();
    expect(view.container.querySelector(".ai-provider-summary")?.textContent).not.toContain("Add provider");

    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    const button = screen.getByRole("button", { name: "Add provider" });
    expect(dialog()!.contains(button)).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());
    expect(screen.queryByRole("button", { name: "Add provider" })).toBeNull();
  });

  it("closes with Escape", async () => {
    await mount();
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());

    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(dialog()).toBeNull());
    expect(settingsButton().textContent).toBe("Settings");
  });

  it("keeps every settings field out of the panel while the modal is closed", async () => {
    const view = await mount();
    expect(screen.queryByText("Endpoint")).toBeNull();
    expect(screen.queryByText("Context (tokens)")).toBeNull();
    expect(screen.queryByText("Toolsets")).toBeNull();
    expect(screen.queryByText("Edit Target")).toBeNull();
    expect(screen.queryByText("Test Connection")).toBeNull();
    expect(screen.queryByRole("button", { name: "Add provider" })).toBeNull();
    expect(view.container.querySelector(".ai-settings-body")).toBeNull();
  });

  it("switches the active provider from the top of the connection column", async () => {
    const view = await mount();
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Provider" }));
    const listbox = await screen.findByRole("listbox");
    const labels = [...listbox.querySelectorAll('[role="option"]')]
      .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent ?? "");
    expect(labels.join("\n")).toContain("Anthropic Claude");
    // The list is the app's listbox, not a native select.
    expect([...view.container.querySelectorAll("select")].some((select) =>
      [...select.options].some((option) => option.textContent === "Anthropic Claude"))).toBe(false);
    const anthropic = [...listbox.querySelectorAll('[role="option"]')]
      .find((option) => option.textContent?.includes("Anthropic Claude")) as HTMLElement;
    fireEvent.click(anthropic);

    await waitFor(() => expect(getActiveProvider().id).toBe("anthropic-claude"));
    expect(getActiveProvider().model).toBe("claude-opus-4-6");
    // The connection column edits the newly active provider, and the composer
    // footer's model selector shows its model.
    const protocol = screen.getAllByRole("combobox")[0] as HTMLSelectElement;
    expect(protocol.value).toBe("anthropic_messages");
    expect(view.container.querySelector(".ai-model-bar")!.textContent).toContain("claude-opus-4-6");
    const keyReads = invokeMock.mock.calls.filter(([command]) => command === "ai_key_status");
    expect(keyReads.at(-1)?.[1]).toEqual({ providerId: "anthropic-claude" });
  });

  it("uses the shared dropdown keyboard model for the provider picker", async () => {
    await mount();
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());

    const trigger = screen.getByRole("button", { name: "Provider" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    const listbox = await screen.findByRole("listbox");
    fireEvent.keyDown(listbox, { key: "ArrowDown" });
    fireEvent.keyDown(listbox, { key: "Enter" });
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
    // Opening selects the current row; one ArrowDown lands on the next provider.
    expect(getActiveProvider().id).toBe("deepseek-v4-pro");
  });

  it("accepts suffixed token counts in the model defaults and rejects text it cannot parse", async () => {
    await mount();
    fireEvent.click(settingsButton());
    const context = (await screen.findByLabelText("Context (tokens)")) as HTMLInputElement;
    // A fresh provider starts at the 1M context default.
    expect(context.value).toBe("1M");

    fireEvent.input(context, { target: { value: "128K" } });
    await waitFor(() => expect(getActiveProvider().contextLength).toBe(128000));
    expect(context.value).toBe("128K");

    fireEvent.input(context, { target: { value: "abc" } });
    expect(context.value).toBe("abc");
    expect(screen.getByText("Enter a token count such as 128K, 1M or 32000.")).toBeTruthy();
    expect(getActiveProvider().contextLength).toBe(128000);

    const output = screen.getByLabelText("Maximum output tokens") as HTMLInputElement;
    // The built-in provider keeps its own 8192 cap; only new rows start at 16K.
    expect(output.value).toBe("8192");
    fireEvent.input(output, { target: { value: "1.5M" } });
    await waitFor(() => expect(getActiveProvider().maxTokens).toBe(1500000));
  });
});
