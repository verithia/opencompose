/**
 * Per-model limits dialog: the bundled values are the baseline, saves go to the
 * user layer, reset clears it, and invalid values name their field.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock } from "./setup";
import { i18n } from "../stores/i18n";
import { AiModelLimits } from "../components/AiModelLimits";
import { defaultLimits, effectiveLimits, estimateTokens } from "../stores/aiLimits";
import { parseTokenCount } from "../ai/tokens";

describe("limits for a model the catalog does not describe", () => {
  it("falls back to conservative defaults instead of no limits", () => {
    const unknown = { provider_id: "acme", model_id: "acme-large", bundled: null, overridden: null };
    expect(effectiveLimits(unknown)).toEqual(defaultLimits());
    expect(effectiveLimits(unknown)).toMatchObject({
      context: 1_000_000,
      max_output: 16_000,
      input: { text: true, image: false, video: false, audio: false },
      price: null,
    });
    // A known model keeps its catalog values; nothing was loaded means nothing to show.
    expect(effectiveLimits(view())).toEqual(bundled);
    expect(effectiveLimits(view(overridden))).toEqual(overridden);
    expect(effectiveLimits(null)).toBeNull();
  });
});

const bundled = {
  context: 128000,
  max_output: 8192,
  input: { text: true, image: false, video: false, audio: false },
  price: null,
};

const overridden = {
  context: 64000,
  max_output: 4096,
  input: { text: true, image: false, video: false, audio: false },
  price: null,
};

function view(override: typeof bundled | null = null) {
  return {
    provider_id: "custom",
    model_id: "third-party-7b",
    bundled,
    overridden: override,
  };
}

function calls(cmd: string) {
  return invokeMock.mock.calls.filter(([name]) => name === cmd);
}

function lastArgs(cmd: string) {
  const found = calls(cmd);
  return found.length ? (found[found.length - 1][1] as Record<string, unknown>) : undefined;
}

async function openDialog() {
  render(() => (
    <AiModelLimits providerId="custom" modelId="third-party-7b" used={12000} />
  ));
  await waitFor(() =>
    expect(calls("ai_model_limits").length).toBeGreaterThan(0),
  );
  fireEvent.click(screen.getByRole("button", { name: "Edit limits" }));
  await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "ai_model_limits") return view();
    if (cmd === "ai_save_model_limits") return "/tmp/models.json";
    if (cmd === "ai_reset_model_limits") return undefined;
    return null;
  });
});

afterEach(cleanup);

describe("model limits dialog", () => {
  it("loads the bundled values and saves edited limits", async () => {
    await openDialog();
    expect(screen.getByText(/Bundled catalog: 128000 context, 8192 max output/)).toBeTruthy();

    fireEvent.input(screen.getByLabelText("Context length (tokens)"), {
      target: { value: "200000" },
    });
    fireEvent.click(screen.getByLabelText("Image"));
    fireEvent.click(screen.getByRole("button", { name: "Save limits" }));

    await waitFor(() => expect(calls("ai_save_model_limits").length).toBe(1));
    expect(lastArgs("ai_save_model_limits")).toEqual({
      providerId: "custom",
      modelId: "third-party-7b",
      limits: {
        context: 200000,
        max_output: 8192,
        input: { text: true, image: true, video: false, audio: false },
        price: null,
      },
    });
    await waitFor(() =>
      expect(screen.getByText("Limits saved; they apply to the next request.")).toBeTruthy(),
    );
  });

  it("resets to the bundled catalog values", async () => {
    let loaded = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "ai_model_limits") {
        loaded += 1;
        return view(loaded === 1 ? overridden : null);
      }
      if (cmd === "ai_reset_model_limits") return undefined;
      if (cmd === "ai_save_model_limits") return "/tmp/models.json";
      return null;
    });
    await openDialog();
    const context = screen.getByLabelText("Context length (tokens)") as HTMLInputElement;
    expect(context.value).toBe("64K");
    fireEvent.click(screen.getByRole("button", { name: "Reset to catalog" }));
    await waitFor(() => expect(calls("ai_reset_model_limits").length).toBe(1));
    expect(lastArgs("ai_reset_model_limits")).toEqual({
      providerId: "custom",
      modelId: "third-party-7b",
    });
    await waitFor(() => expect(context.value).toBe("128K"));
    expect(parseTokenCount(context.value)).toBe(128000);
  });

  it("names the field when the draft is invalid and never saves it", async () => {
    await openDialog();
    fireEvent.input(screen.getByLabelText("Context length (tokens)"), {
      target: { value: "0" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save limits" }));
    await waitFor(() =>
      expect(screen.getByText("Context length must be greater than 0.")).toBeTruthy(),
    );
    expect(calls("ai_save_model_limits").length).toBe(0);

    fireEvent.input(screen.getByLabelText("Context length (tokens)"), {
      target: { value: "1000" },
    });
    fireEvent.input(screen.getByLabelText("Output token limit"), {
      target: { value: "2000" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save limits" }));
    await waitFor(() =>
      expect(
        screen.getByText("Output limit must not exceed the context length."),
      ).toBeTruthy(),
    );
    expect(calls("ai_save_model_limits").length).toBe(0);
  });

  it("accepts a suffixed context and keeps unparseable text out of the draft", async () => {
    await openDialog();
    const context = screen.getByLabelText("Context length (tokens)") as HTMLInputElement;
    // The raw text stays while it cannot parse yet, with the reason below it.
    fireEvent.input(context, { target: { value: "1.5" } });
    expect(context.value).toBe("1.5");
    expect(screen.getByText("Enter a token count such as 128K, 1M or 32000.")).toBeTruthy();
    // A valid count commits and is re-displayed in its formatted form.
    fireEvent.input(context, { target: { value: "1.5M" } });
    fireEvent.blur(context);
    expect(context.value).toBe("1.5M");
    fireEvent.click(screen.getByRole("button", { name: "Save limits" }));
    await waitFor(() => expect(calls("ai_save_model_limits").length).toBe(1));
    const limits = lastArgs("ai_save_model_limits") as { limits: { context: number } };
    expect(limits.limits.context).toBe(1500000);
  });

  it("warns when the estimated prompt exceeds the context limit", async () => {
    render(() => <AiModelLimits providerId="custom" modelId="third-party-7b" used={200000} />);
    await waitFor(() => expect(calls("ai_model_limits").length).toBeGreaterThan(0));
    await waitFor(() =>
      expect(
        screen.getByText(
          "The estimated prompt exceeds the context limit; the provider will likely reject it.",
        ),
      ).toBeTruthy(),
    );
  });

  it("groups the context readout's digits", async () => {
    render(() => <AiModelLimits providerId="custom" modelId="third-party-7b" used={3387} />);
    await waitFor(() =>
      expect(screen.getByText("Context ≈ 3,387 / 128,000 tokens")).toBeTruthy(),
    );
  });

  it("estimates tokens from characters and says so", () => {
    expect(estimateTokens("")).toBe(0);
    expect(estimateTokens("abcd")).toBe(1);
    expect(estimateTokens("abcde")).toBe(2);
    expect(estimateTokens("音")).toBe(1);
    expect(i18n.t("ai.limits.heuristic")).toContain("tokenizer");
  });

  it("has every limit string in English, simplified and traditional Chinese", () => {
    const keys = i18n.keys().filter((key) => key.startsWith("ai.limits."));
    expect(keys.length).toBeGreaterThan(20);
    for (const key of keys) {
      expect(i18n.has("zh-Hans", key), key).toBe(true);
      expect(i18n.has("zh-Hant", key), key).toBe(true);
    }
  });
});
