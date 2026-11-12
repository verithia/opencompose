/**
 * Add-provider modal: the panel opens a wide dialog that fetches the provider's
 * model list and configures each enabled model on a single line. The provider
 * file keeps only ids; per-model settings go to the existing override file and
 * a typed key only to the credential store.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { DEFAULT_PROVIDERS, aiConfig, type AiProvider } from "../ai/config";

const ACME = {
  id: "acme-gateway",
  name: "Acme Gateway",
  kind: "openai_chat_completions",
  wire: "openai-chat-completions",
  auth: "api_key",
  endpoint: "https://llm.acme.test/v1/chat/completions",
  model: "acme-large",
  models: ["acme-large", "acme-small"],
  context_length: 64000,
  max_tokens: 4096,
  temperature: 0.3,
  capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: false, streaming: true },
};

/** The same shape `toRust` produces, so the file round-trip is realistic. */
function rustProvider(provider: AiProvider) {
  return {
    id: provider.id,
    name: provider.name,
    kind: provider.kind,
    ...(provider.wire ? { wire: provider.wire } : {}),
    auth: provider.auth,
    ...(provider.authProvider ? { auth_provider: provider.authProvider } : {}),
    ...(provider.oauthAllowedHosts?.length ? { oauth_allowed_hosts: provider.oauthAllowedHosts } : {}),
    endpoint: provider.endpoint,
    model: provider.model,
    ...(provider.models?.length ? { models: provider.models } : {}),
    context_length: provider.contextLength,
    max_tokens: provider.maxTokens,
    temperature: provider.temperature,
    capabilities: {
      text: provider.capabilities.text,
      audio_in: provider.capabilities.audioIn,
      audio_out: provider.capabilities.audioOut,
      native_tool_calling: provider.capabilities.nativeToolCalling,
      streaming: provider.capabilities.streaming,
    },
  };
}

/** The panel's own startup saves the defaults, so the file holds them too. */
const SAVED_CONFIG = {
  version: 2,
  active_provider: "acme-gateway",
  providers: [...DEFAULT_PROVIDERS.map(rustProvider), ACME],
};

let saved = false;

function mockBackend(handlers: Record<string, (args: unknown) => unknown> = {}) {
  saved = false;
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    if (handlers[cmd]) return handlers[cmd](args);
    if (cmd === "load_ai_config") {
      // The first read is the panel's own startup; after the add the backend
      // returns the config that now contains the new provider.
      if (!saved) return { version: 2, active_provider: "", providers: [] };
      return SAVED_CONFIG;
    }
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return { version: "test", providers: [] };
    if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    if (cmd === "ai_fetch_models") {
      return { models: [{ id: "acme-large" }, { id: "acme-small" }], total: 2 };
    }
    if (cmd === "ai_add_provider") {
      saved = true;
      return { provider_id: "acme-gateway", config: SAVED_CONFIG };
    }
    if (cmd === "ai_set_key") return undefined;
    return makeSnapshot();
  });
}

async function openModal() {
  await store.initProject();
  const view = render(() => <><AiPanel /><PreferencesHost /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  fireEvent.click(screen.getByRole("button", { name: "Provider settings" }));
  await waitFor(() => expect(screen.getByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy());
  // Add provider lives in the settings dialog's connection column.
  fireEvent.click(screen.getByRole("button", { name: "Add provider" }));
  await waitFor(() => expect(screen.getByRole("dialog", { name: "New provider" })).toBeTruthy());
  return view;
}

function fillProvider(name = "Acme Gateway", endpoint = "https://llm.acme.test/v1/chat/completions") {
  fireEvent.input(screen.getByLabelText("Name"), { target: { value: name } });
  fireEvent.input(screen.getByLabelText(/Base URL/), { target: { value: endpoint } });
}

function calls(name: string) {
  return invokeMock.mock.calls.filter(([command]) => command === name);
}

/** The modal lives in a portal, so its rows are outside the panel container. */
function rows(): Element[] {
  return [...document.querySelectorAll(".ai-provider-model-row:not(.ai-provider-model-head)")];
}

beforeEach(() => {
  invokeMock.mockReset();
  mockBackend();
});

afterEach(cleanup);

describe("add provider modal", () => {
  it("opens from the panel button with focus in the first field and closes on Escape", async () => {
    const view = await openModal();
    expect(rows().length).toBe(0);
    expect(screen.getByText("No models yet. Fetch the list or add ids by hand.")).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Name")));
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New provider" })).toBeNull());
    expect(view.container.querySelector(".ai-provider-modal")).toBeFalsy();
  });

  it("renders one compact row per fetched model and enables the first", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));

    await waitFor(() => expect(rows().length).toBe(2));
    expect(calls("ai_fetch_models")[0][1]).toEqual({
      endpoint: "https://llm.acme.test/v1/chat/completions",
      wire: "openai-chat-completions",
      auth: "api_key",
      key: null,
    });
    expect(screen.getByText("acme-large")).toBeTruthy();
    expect(screen.getByText("acme-small")).toBeTruthy();
    expect(screen.getByLabelText("Include acme-large")).toBeChecked();
    expect(screen.getByLabelText("Include acme-small")).not.toBeChecked();
    expect(screen.getByText("1/2 enabled")).toBeTruthy();
    // A single line per model: toggle, name, context, output, four modalities.
    const row = rows()[1] as HTMLElement;
    expect(row.querySelectorAll("input[type=number]").length).toBe(0);
    expect(row.querySelectorAll("input[type=text]").length).toBe(2);
    expect(row.querySelectorAll(".ai-provider-mod").length).toBe(4);
  });

  it("starts every new model row at 1M context and 16K output", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));

    for (const model of ["acme-large", "acme-small"]) {
      expect((screen.getByLabelText(`Context length for ${model}`) as HTMLInputElement).value).toBe("1M");
      expect((screen.getByLabelText(`Maximum output tokens for ${model}`) as HTMLInputElement).value).toBe("16K");
    }
  });

  it("stores a suffixed context and keeps the row in place while typing", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));
    const context = screen.getByLabelText("Context length for acme-large") as HTMLInputElement;
    expect(context.value).toBe("1M");

    context.focus();
    fireEvent.input(context, { target: { value: "1" } });
    // The row updates in place, so the next keystroke lands in the same field.
    expect(document.activeElement).toBe(screen.getByLabelText("Context length for acme-large"));
    fireEvent.input(context, { target: { value: "128K" } });
    fireEvent.blur(context);
    expect(context.value).toBe("128K");

    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    const draft = (calls("ai_add_provider")[0][1] as {
      draft: { limits: Record<string, { context: number }> };
    }).draft;
    expect(draft.limits["acme-large"].context).toBe(128000);
  });

  it("reports unparseable text and never persists it", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));
    const context = screen.getByLabelText("Context length for acme-large") as HTMLInputElement;

    fireEvent.input(context, { target: { value: "abc" } });
    expect(context.value).toBe("abc");
    await waitFor(() =>
      expect(
        screen.getByText("Check acme-large: Enter a token count such as 128K, 1M or 32000."),
      ).toBeTruthy(),
    );
    // The row blocks creation instead of being saved with the previous value.
    expect(
      (screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement).disabled,
    ).toBe(true);

    fireEvent.input(context, { target: { value: "200K" } });
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    const draft = (calls("ai_add_provider")[0][1] as {
      draft: { limits: Record<string, { context: number }> };
    }).draft;
    expect(draft.limits["acme-large"].context).toBe(200000);
  });

  it("persists the enabled ids, per-model settings and the key path", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(screen.getByLabelText("Include acme-small")).toBeTruthy());
    fireEvent.click(screen.getByLabelText("Include acme-small"));
    fireEvent.input(screen.getByLabelText("Context length for acme-small"), { target: { value: "64000" } });
    fireEvent.input(screen.getByLabelText("Maximum output tokens for acme-small"), { target: { value: "8192" } });
    fireEvent.click(screen.getByLabelText("Audio input for acme-small"));
    fireEvent.input(screen.getByLabelText("API key"), { target: { value: "sk-test-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));

    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    const draft = (calls("ai_add_provider")[0][1] as { draft: Record<string, unknown> }).draft;
    expect(draft.name).toBe("Acme Gateway");
    expect(draft.endpoint).toBe("https://llm.acme.test/v1/chat/completions");
    expect(draft.models).toEqual(["acme-large", "acme-small"]);
    const limits = draft.limits as Record<string, { context: number; max_output: number; input: Record<string, boolean> }>;
    expect(limits["acme-small"].context).toBe(64000);
    expect(limits["acme-small"].max_output).toBe(8192);
    expect(limits["acme-small"].input).toEqual({ text: true, image: false, video: false, audio: true });
    // The key never travels inside the provider draft.
    expect(JSON.stringify(draft)).not.toContain("sk-test-secret");
    await waitFor(() => expect(calls("ai_set_key").length).toBe(1));
    expect(calls("ai_set_key")[0][1]).toEqual({ providerId: "acme-gateway", key: "sk-test-secret" });
  });

  it("updates the panel summary once the provider is saved", async () => {
    const view = await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(screen.getByLabelText("Include acme-large")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));

    await waitFor(() => expect(aiConfig.config.activeProviderId).toBe("acme-gateway"));
    expect(view.container.querySelector(".ai-provider-summary")!.textContent).toContain("Acme Gateway");
    // The built-in entries plus the one this dialog just created.
    expect(view.container.querySelector(".ai-provider-summary")!.textContent)
      .toContain(`${DEFAULT_PROVIDERS.length + 1} providers`);
    // The composer footer shows the model; the provider name lives in the summary.
    expect(view.container.querySelector(".ai-model-bar")!.textContent).toContain("acme-large");
  });

  it("keeps the manual path for a provider with no model endpoint", async () => {
    mockBackend({
      ai_fetch_models: () => {
        throw new Error("No model list at this address (HTTP 404). Check the base URL.");
      },
    });
    await openModal();
    fillProvider("Local Gateway", "http://localhost:1234/v1");
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(screen.getByText(/HTTP 404/)).toBeTruthy());
    // The failure does not block a hand-written id.
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "local-model" } });
    fireEvent.keyDown(screen.getByLabelText("Model id"), { key: "Enter" });
    await waitFor(() => expect(screen.getByText("local-model")).toBeTruthy());
    expect(screen.queryByText(/HTTP 404/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    expect((calls("ai_add_provider")[0][1] as { draft: { models: string[] } }).draft.models).toEqual(["local-model"]);
  });

  it("does not swallow the panel's keyboard shortcuts while open", async () => {
    await openModal();
    const seen: string[] = [];
    const listener = (event: KeyboardEvent) => seen.push(event.key);
    document.addEventListener("keydown", listener);
    try {
      fireEvent.keyDown(screen.getByLabelText("Name"), { key: "d" });
      expect(seen).toEqual(["d"]);
      // Escape is the modal's own key and does not reach the app's handler.
      fireEvent.keyDown(document.activeElement!, { key: "Escape" });
      await waitFor(() => expect(screen.queryByRole("dialog", { name: "New provider" })).toBeNull());
      expect(seen).toEqual(["d"]);
    } finally {
      document.removeEventListener("keydown", listener);
    }
  });

  it("refuses OAuth for a new provider and explains why", async () => {
    await openModal();
    fireEvent.click(screen.getByRole("button", { name: "Authentication" }));
    const listbox = await screen.findByRole("listbox");
    const oauth = [...listbox.querySelectorAll('[role="option"]')]
      .find((option) => option.textContent?.includes("OAuth")) as HTMLElement;
    fireEvent.click(oauth);
    await waitFor(() =>
      expect(screen.getByText(/OAuth is available only for providers from the catalog/)).toBeTruthy(),
    );
    expect((screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("offers exactly the protocols this build speaks", async () => {
    await openModal();
    fireEvent.click(screen.getByRole("button", { name: "Provider protocol" }));
    const listbox = await screen.findByRole("listbox");
    const labels = [...listbox.querySelectorAll('[role="option"]')].map((option) => option.textContent?.replace(/^✓/, "").trim());
    expect(labels).toEqual([
      "OpenAI Chat Completions",
      "OpenAI Responses",
      "Anthropic Messages",
      "Gemini Interactions",
      "Gemini GenerateContent",
    ]);
  });

  it("shows a backend error without losing the form", async () => {
    mockBackend({
      ai_add_provider: () => {
        throw new Error("Add at least one model id.");
      },
    });
    await openModal();
    fillProvider();
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "acme-large" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.getByText("acme-large")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(screen.getByText("Add at least one model id.")).toBeTruthy());
    expect(screen.getByRole("dialog", { name: "New provider" })).toBeTruthy();
  });

  it("prefills the limits a fetched model reports and keeps them editable", async () => {
    mockBackend({
      ai_fetch_models: () => ({
        models: [
          { id: "gemini-large", context: 2_000_000, max_output: 64_000 },
          { id: "gemini-plain" },
        ],
        total: 2,
      }),
    });
    await openModal();
    fillProvider("Gemini", "https://generativelanguage.googleapis.com/v1beta/interactions");
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));

    const context = screen.getByLabelText("Context length for gemini-large") as HTMLInputElement;
    expect(context.value).toBe("2M");
    expect((screen.getByLabelText("Maximum output tokens for gemini-large") as HTMLInputElement).value)
      .toBe("64K");
    // A model the listing reports no limits for keeps the dialog defaults.
    expect((screen.getByLabelText("Context length for gemini-plain") as HTMLInputElement).value).toBe("1M");
    expect((screen.getByLabelText("Maximum output tokens for gemini-plain") as HTMLInputElement).value)
      .toBe("16K");

    // The prefill is a starting point: an edited value wins when saving.
    fireEvent.input(context, { target: { value: "1.5M" } });
    fireEvent.blur(context);
    expect(context.value).toBe("1.5M");
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    const draft = (calls("ai_add_provider")[0][1] as {
      draft: { limits: Record<string, { context: number }> };
    }).draft;
    expect(draft.limits["gemini-large"].context).toBe(1_500_000);
  });

  it("says when the listing was cut at the backend cap", async () => {
    mockBackend({
      ai_fetch_models: () => ({ models: [{ id: "acme-large" }, { id: "acme-small" }], total: 12 }),
    });
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));
    expect(screen.getByText("Showing the first 2 of 12 models.")).toBeTruthy();
  });
});

/** A promise the test resolves when it wants the pending command to answer. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("add provider dialog rows", () => {
  it("keeps Create disabled while no row is enabled", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));
    const create = () => screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement;
    expect(create().disabled).toBe(false);

    fireEvent.click(screen.getByLabelText("Include acme-large"));
    await waitFor(() => expect(screen.getByText("0/2 enabled")).toBeTruthy());
    expect(create().disabled).toBe(true);
    // The hint that explains the first enabled model is the default is gone too.
    expect(screen.queryByText("The first enabled model becomes the default.")).toBeNull();

    fireEvent.click(screen.getByLabelText("Include acme-small"));
    await waitFor(() => expect(screen.getByText("1/2 enabled")).toBeTruthy());
    expect(create().disabled).toBe(false);
    fireEvent.click(create());
    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    const draft = (calls("ai_add_provider")[0][1] as { draft: { models: string[] } }).draft;
    expect(draft.models).toEqual(["acme-small"]);
  });

  it("refuses more than 32 enabled rows before anything is written", async () => {
    mockBackend({
      ai_fetch_models: () => ({
        models: Array.from({ length: 33 }, (_, index) => ({ id: `m${String(index).padStart(2, "0")}` })),
        total: 33,
      }),
    });
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(33));
    for (const checkbox of screen.getAllByRole("checkbox")) {
      if (!(checkbox as HTMLInputElement).checked) fireEvent.click(checkbox);
    }
    await waitFor(() => expect(screen.getByText("33/33 enabled")).toBeTruthy());
    expect(screen.getByText("At most 32 models per provider.")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement).disabled)
      .toBe(true);
    expect(calls("ai_add_provider").length).toBe(0);
  });

  it("carries each modality chip into the per-model override", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));

    for (const modality of ["Image", "Video", "Audio"]) {
      fireEvent.click(screen.getByLabelText(`${modality} input for acme-large`));
    }
    for (const modality of ["Image", "Video", "Audio"]) {
      await waitFor(() =>
        expect(screen.getByLabelText(`${modality} input for acme-large`).getAttribute("aria-pressed"))
          .toBe("true"),
      );
    }
    // Text stays on: it is the one modality a row starts with.
    expect(screen.getByLabelText("Text input for acme-large").getAttribute("aria-pressed"))
      .toBe("true");

    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(calls("ai_add_provider").length).toBe(1));
    const limits = (calls("ai_add_provider")[0][1] as {
      draft: { limits: Record<string, { input: Record<string, boolean> }> };
    }).draft.limits;
    expect(limits["acme-large"].input).toEqual({ text: true, image: true, video: true, audio: true });
    // A disabled row carries no override at all.
    expect(limits["acme-small"]).toBeUndefined();
  });

  it("refuses a row whose every input modality is off", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));
    fireEvent.click(screen.getByLabelText("Text input for acme-large"));
    await waitFor(() =>
      expect(screen.getByText("Check acme-large: Choose at least one input modality.")).toBeTruthy(),
    );
    expect((screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement).disabled)
      .toBe(true);
    expect(calls("ai_add_provider").length).toBe(0);

    fireEvent.click(screen.getByLabelText("Text input for acme-large"));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement)
        .toHaveProperty("disabled", false),
    );
  });

  it("refuses a row whose output exceeds its context", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));
    const context = screen.getByLabelText("Context length for acme-large") as HTMLInputElement;

    fireEvent.input(context, { target: { value: "8K" } });
    await waitFor(() =>
      expect(screen.getByText("Check acme-large: Output limit must not exceed the context length."))
        .toBeTruthy(),
    );
    expect((screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement).disabled)
      .toBe(true);

    fireEvent.input(context, { target: { value: "32K" } });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement)
        .toHaveProperty("disabled", false),
    );
    expect(screen.queryByText(/Output limit must not exceed/)).toBeNull();
  });

  it("trims a manual id, adds it once and enables it when it is already listed", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(rows().length).toBe(2));

    // A listed but disabled id becomes enabled instead of appearing twice.
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "  acme-small  " } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.getByLabelText("Include acme-small")).toBeChecked());
    expect(rows().length).toBe(2);

    // A new id is trimmed, and repeating it changes nothing.
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "  my-model  " } });
    fireEvent.keyDown(screen.getByLabelText("Model id"), { key: "Enter" });
    await waitFor(() => expect(rows().length).toBe(3));
    expect(screen.getByText("my-model")).toBeTruthy();
    expect((screen.getByLabelText("Model id") as HTMLInputElement).value).toBe("");
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "my-model" } });
    fireEvent.keyDown(screen.getByLabelText("Model id"), { key: "Enter" });
    await waitFor(() => expect(rows().length).toBe(3));
    // Enter on an empty field adds nothing.
    fireEvent.keyDown(screen.getByLabelText("Model id"), { key: "Enter" });
    expect(rows().length).toBe(3);
  });

  it("hands an over-long manual id to the backend instead of truncating it", async () => {
    const longId = `m-${"x".repeat(210)}`;
    mockBackend({
      ai_add_provider: () => {
        throw new Error("Model ids must be at most 200 characters");
      },
    });
    await openModal();
    fillProvider();
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: longId } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(rows().length).toBe(1));
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(screen.getByText(/at most 200 characters/)).toBeTruthy());
    const draft = (calls("ai_add_provider")[0][1] as { draft: { models: string[] } }).draft;
    expect(draft.models).toEqual([longId]);
  });

  it("maps every listing failure to a readable message and never echoes the key", async () => {
    const messages = [
      "The provider rejected the API key (HTTP 401).",
      "The provider answered HTTP 500 when listing models.",
      "The provider's model list is not JSON.",
      "The provider returned no models. Add model ids by hand.",
      "Could not connect to the provider. Check the base URL.",
      "Listing models timed out. Check the base URL and network.",
    ];
    const view = await openModal();
    fillProvider();
    fireEvent.input(screen.getByLabelText("API key"), { target: { value: "sk-listing-secret" } });
    for (const message of messages) {
      mockBackend({
        ai_fetch_models: () => {
          throw new Error(message);
        },
      });
      fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
      await waitFor(() => expect(screen.getByText(message)).toBeTruthy());
      // The failure keeps the hand-written path open and leaks no key.
      expect(view.container.ownerDocument.body.textContent).not.toContain("sk-listing-secret");
      expect(rows().length).toBe(0);
    }
  });

  it("closes on Escape while a fetch is in flight and creates nothing", async () => {
    const pending = deferred<{ models: { id: string }[]; total: number }>();
    mockBackend({ ai_fetch_models: () => pending.promise });
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Fetching…" })).toBeTruthy());
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New provider" })).toBeNull());
    expect(calls("ai_add_provider").length).toBe(0);
    expect(calls("ai_set_key").length).toBe(0);
    // The answer arrives after the dialog is gone and changes nothing.
    pending.resolve({ models: [{ id: "late-model" }], total: 1 });
    await Promise.resolve();
  });

  it("cannot be closed while the provider is being created", async () => {
    const pending = deferred<{ provider_id: string; config: unknown }>();
    mockBackend({ ai_add_provider: () => pending.promise });
    const view = await openModal();
    fillProvider();
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "acme-large" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(rows().length).toBe(1));
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Saving…" })).toBeTruthy());

    const dialog = screen.getByRole("dialog", { name: "New provider" });
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    fireEvent.click(dialog.querySelector(".ai-provider-close") as HTMLElement);
    fireEvent.click(document.querySelector(".ai-provider-overlay")!);
    expect(screen.getByRole("dialog", { name: "New provider" })).toBeTruthy();
    expect(view.container.querySelector(".ai-provider-modal")).toBeFalsy();

    pending.resolve({ provider_id: "acme-gateway", config: SAVED_CONFIG });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New provider" })).toBeNull());
    expect(calls("ai_add_provider").length).toBe(1);
  });

  it("retries only the key when the provider was created but the key failed", async () => {
    let keyCalls = 0;
    mockBackend({
      ai_set_key: () => {
        keyCalls += 1;
        if (keyCalls === 1) throw new Error("Could not save to OS credential store");
        return undefined;
      },
    });
    await openModal();
    fillProvider();
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "acme-large" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(rows().length).toBe(1));
    fireEvent.input(screen.getByLabelText("API key"), { target: { value: "sk-key-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));

    await waitFor(() =>
      expect(screen.getByText(/Acme Gateway was added, but its key was not saved/)).toBeTruthy(),
    );
    // The provider exists, so the dialog stays open on the key error only.
    expect(screen.getByRole("dialog", { name: "New provider" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create provider" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New provider" })).toBeNull());
    // A second create would have made `acme-gateway-2`; there is exactly one.
    expect(calls("ai_add_provider").length).toBe(1);
    expect(calls("ai_set_key").length).toBe(2);
    expect(calls("ai_set_key")[1][1]).toEqual({ providerId: "acme-gateway", key: "sk-key-secret" });
  });

  it("keeps the form incomplete until name, endpoint and one row exist", async () => {
    await openModal();
    const create = () => screen.getByRole("button", { name: "Create provider" }) as HTMLButtonElement;
    // A one-character name is enough; an empty one is not.
    fireEvent.input(screen.getByLabelText("Model id"), { target: { value: "acme-large" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(rows().length).toBe(1));
    expect(create().disabled).toBe(true);
    fireEvent.input(screen.getByLabelText(/Base URL/), { target: { value: "https://llm.acme.test/v1/chat/completions" } });
    expect(create().disabled).toBe(true);
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "A" } });
    await waitFor(() => expect(create().disabled).toBe(false));
  });

  it("follows the selected protocol into the next fetch", async () => {
    await openModal();
    fillProvider();
    fireEvent.click(screen.getByRole("button", { name: "Provider protocol" }));
    const listbox = await screen.findByRole("listbox");
    const anthropic = [...listbox.querySelectorAll('[role="option"]')]
      .find((option) => option.textContent?.includes("Anthropic Messages")) as HTMLElement;
    fireEvent.click(anthropic);
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(calls("ai_fetch_models").length).toBe(1));
    expect((calls("ai_fetch_models")[0][1] as { wire: string }).wire).toBe("anthropic-messages");
    // The endpoint is sent exactly as typed; the backend derives the /models base.
    fireEvent.input(screen.getByLabelText(/Base URL/), {
      target: { value: "https://api.anthropic.com/v1/messages/" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Fetch models" }));
    await waitFor(() => expect(calls("ai_fetch_models").length).toBe(2));
    expect((calls("ai_fetch_models")[1][1] as { endpoint: string }).endpoint)
      .toBe("https://api.anthropic.com/v1/messages/");
  });
});
