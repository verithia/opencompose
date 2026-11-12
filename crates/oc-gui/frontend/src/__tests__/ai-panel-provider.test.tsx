/**
 * The AI panel header carries the model and thinking-effort selectors: the
 * separate Provider/Model row is gone, and provider switching survives because
 * a model belongs to a provider (picking one switches both) and the settings
 * dialog lists the providers too.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { getActiveProvider } from "../ai/config";
import { aiLimits } from "../stores/aiLimits";

const CAPABILITIES = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

const CATALOG = {
  version: "fixture",
  providers: [
    {
      id: "deepseek",
      name: "DeepSeek",
      kind: "openai_chat_completions",
      hosts: ["api.deepseek.com"],
      auth_providers: [],
      models: [
        { id: "deepseek-flash", name: "DeepSeek Flash", context: 900000, max_output: 8192, capabilities: CAPABILITIES, status: "active" },
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro", context: 500000, max_output: 4096, capabilities: CAPABILITIES, status: "active" },
      ],
    },
    {
      id: "google",
      name: "Google",
      kind: "gemini_interactions",
      hosts: ["generativelanguage.googleapis.com"],
      auth_providers: [],
      models: [
        {
          id: "gemini-4-argon",
          name: "Gemini 4 Argon",
          context: 200000,
          max_output: 32000,
          thinking_levels: ["low", "medium", "high"],
          capabilities: CAPABILITIES,
          status: "active",
        },
      ],
    },
  ],
};

function mockBackend(limits?: (modelId: string) => unknown) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "load_ai_config") return aiConfigWire();
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return CATALOG;
    if (cmd === "ai_model_limits") {
      const model = String((args as Record<string, unknown> | undefined)?.modelId ?? "");
      return limits ? limits(model) : { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    }
    return makeSnapshot();
  });
}

/** A window the test resolves when it wants the limits answer to land. */
function deferred<T = unknown>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((ok) => { resolve = ok; });
  return { promise, resolve };
}

/** The merged limits of one model, as the backend reports them. */
function limitsFor(context: number) {
  return {
    provider_id: "deepseek",
    model_id: "m",
    bundled: { context, max_output: 4096, input: { text: true, image: false, video: false, audio: false }, price: null },
    overridden: null,
  };
}

async function mount() {
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  return view;
}

function modelButton(): HTMLElement {
  return screen.getByRole("button", { name: "Model" });
}

function effortButton(): HTMLElement | null {
  return screen.queryByRole("button", { name: "Thinking effort" });
}

/** Opens the model list and clicks the option whose label carries `id`. */
/** Option text without the selected-check glyph. */
function optionLabels(listbox: HTMLElement): string[] {
  return [...listbox.querySelectorAll('[role="option"]')]
    .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent ?? "");
}

async function pickModel(id: string) {
  fireEvent.click(modelButton());
  const listbox = await screen.findByRole("listbox");
  const option = [...listbox.querySelectorAll('[role="option"]')]
    .find((entry) => entry.textContent?.includes(id)) as HTMLElement;
  fireEvent.click(option);
}

beforeEach(() => {
  invokeMock.mockReset();
  mockBackend();
});

afterEach(cleanup);

describe("AI panel header selectors", () => {
  it("renders the model under the composer and keeps it out of the header", async () => {
    const view = await mount();
    const header = view.container.querySelector(".ai-panel-header")!;
    expect(header.textContent).toContain("AI");
    // The header names the conversation; the model is the composer's footer.
    expect(header.querySelectorAll(".oc-dropdown")).toHaveLength(0);
    const modelBar = view.container.querySelector(".ai-model-bar");
    expect(modelBar).toBeTruthy();
    expect(modelBar!.contains(modelButton())).toBe(true);
    expect(modelButton().textContent).toContain("deepseek-flash");
    // deepseek-flash declares no thinking levels: the selector is absent, not disabled.
    expect(effortButton()).toBeNull();
    expect(view.container.querySelector(".ai-picker-row")).toBeNull();
    expect(screen.getByRole("button", { name: "Provider settings" })).toBeTruthy();
  });

  it("lists every provider's models and switches provider and model in one action", async () => {
    await mount();
    fireEvent.click(modelButton());
    const listbox = await screen.findByRole("listbox");
    // Each option names the provider and the model: a bare id in a narrow list
    // does not say which provider serves it.
    expect(optionLabels(listbox)).toEqual([
      "deepseek-v4-flash/deepseek-flash",
      "deepseek-v4-flash/deepseek-v4-pro",
      "gemini-2.5-pro/gemini-4-argon",
    ]);
    const argon = [...listbox.querySelectorAll('[role="option"]')]
      .find((option) => option.textContent?.includes("gemini-4-argon")) as HTMLElement;
    fireEvent.click(argon);

    await waitFor(() => expect(getActiveProvider().id).toBe("gemini-2.5-pro"));
    expect(getActiveProvider().model).toBe("gemini-4-argon");
    // The catalog entry's limits move with the model.
    expect(getActiveProvider().contextLength).toBe(200000);
    expect(getActiveProvider().maxTokens).toBe(32000);
    expect(modelButton().textContent).toContain("gemini-4-argon");
    expect(effortButton()!.textContent).toContain("medium");
  });

  it("ignores a limits answer that belongs to a model the panel left", async () => {
    const late = deferred();
    mockBackend((model) => (model === "gemini-4-argon" ? late.promise : limitsFor(model === "deepseek-flash" ? 1_000 : 2_000)));
    await mount();
    await waitFor(() => expect(aiLimits.activeContext()).toBe(1_000));

    // The gemini request is still on its way when the panel picks another
    // model, whose own limits arrive first.
    await pickModel("gemini-4-argon");
    await pickModel("deepseek-v4-pro");
    await waitFor(() => expect(aiLimits.activeContext()).toBe(2_000));

    // The late answer would budget the next request with another model's window.
    late.resolve(limitsFor(200_000));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(aiLimits.activeContext()).toBe(2_000);
  });

  it("offers the declared levels and clears a stale one when the model has none", async () => {
    await mount();
    await pickModel("gemini-4-argon");
    await waitFor(() => expect(effortButton()).toBeTruthy());

    fireEvent.click(effortButton()!);
    const levels = await screen.findByRole("listbox");
    expect(optionLabels(levels)).toEqual(["low", "medium", "high"]);
    const high = [...levels.querySelectorAll('[role="option"]')]
      .find((option) => option.textContent?.includes("high")) as HTMLElement;
    fireEvent.click(high);
    await waitFor(() => expect(getActiveProvider().thinkingLevel).toBe("high"));
    expect(effortButton()!.textContent).toContain("high");

    await pickModel("deepseek-v4-pro");
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-v4-pro"));
    expect(getActiveProvider().id).toBe("deepseek-v4-flash");
    // The hidden setting never keeps a level the model cannot take.
    expect(getActiveProvider().thinkingLevel).toBe("medium");
    expect(getActiveProvider().contextLength).toBe(500000);
    expect(getActiveProvider().maxTokens).toBe(4096);
    expect(effortButton()).toBeNull();
  });
});
