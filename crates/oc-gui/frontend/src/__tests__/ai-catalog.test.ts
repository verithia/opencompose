import { describe, expect, it } from "vitest";
import { catalogProviderFor, isCustomEndpoint, modelFor, modelsForWire, type Catalog } from "../ai/catalog";

const catalog: Catalog = {
  version: "fixture",
  providers: [
    {
      id: "openai",
      name: "OpenAI",
      wire: "openai-responses",
      hosts: ["api.openai.com"],
      auth_providers: [],
      models: [
        {
          id: "gpt-6.1-sol",
          name: "GPT-6.1 Sol",
          context: 1050000,
          max_output: 128000,
          capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true },
          status: "active",
        },
      ],
    },
    {
      id: "ollama",
      name: "Ollama",
      wire: "openai-chat-completions",
      hosts: ["localhost"],
      auth_providers: [],
      models: [],
    },
  ],
};

describe("bundled catalog helpers", () => {
  it("looks models up by protocol and rejects an unknown id", () => {
    expect(modelsForWire(catalog, "openai-responses").map((m: { id: string }) => m.id)).toEqual(["gpt-6.1-sol"]);
    expect(modelFor(catalog, "openai-responses", "gpt-6.1-sol")?.context).toBe(1050000);
    expect(modelFor(catalog, "openai-responses", "unknown")).toBeUndefined();
    expect(modelsForWire(catalog, "gemini-interactions")).toEqual([]);
    expect(modelsForWire(null, "openai-responses")).toEqual([]);
  });

  it("finds the catalog entry that serves a saved provider by protocol and host", () => {
    expect(catalogProviderFor(catalog, {
      wire: "openai-responses", endpoint: "https://api.openai.com/v1/responses",
    })?.id).toBe("openai");
    // The same host on another protocol is a different provider.
    expect(catalogProviderFor(catalog, {
      wire: "openai-chat-completions", endpoint: "https://api.openai.com/v1/responses",
    })).toBeUndefined();
    expect(catalogProviderFor(catalog, {
      wire: "openai-responses", endpoint: "https://proxy.example/v1/responses",
    })).toBeUndefined();
    expect(catalogProviderFor(null, {
      wire: "openai-responses", endpoint: "https://api.openai.com/v1/responses",
    })).toBeUndefined();
  });

  it("treats catalog vendors as strict and custom or local endpoints as free-form", () => {
    expect(isCustomEndpoint(catalog, "https://api.openai.com/v1/responses")).toBe(false);
    expect(isCustomEndpoint(catalog, "https://example.test/v1/responses")).toBe(true);
    expect(isCustomEndpoint(catalog, "http://localhost:11434/v1/chat/completions")).toBe(true);
    expect(isCustomEndpoint(null, "https://api.openai.com/v1/responses")).toBe(true);
  });
});
