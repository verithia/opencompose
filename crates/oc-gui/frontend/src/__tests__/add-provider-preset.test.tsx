import { describe, expect, it, vi } from "vitest";
import { render } from "@solidjs/testing-library";
import { AddProviderModal } from "../components/AddProviderModal";
import { chooseDropdown } from "./setup";
import { i18n } from "../stores/i18n";
import type { Catalog } from "../ai/catalog";

const CATALOG: Catalog = {
  version: "test",
  providers: [{
    id: "openrouter",
    name: "OpenRouter",
    wire: "openai-chat-completions",
    base_url: "https://openrouter.ai/api/v1",
    path: "chat/completions",
    hosts: ["openrouter.ai"],
    auth_providers: [],
    models: [{
      id: "acme/large", name: "Acme Large", context: 200000, max_output: 8192,
      capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true },
      status: "active",
    }],
  }],
};

describe("add-provider presets", () => {
  it("fills the form from a catalog provider", () => {
    i18n.setLocale("en");
    render(() => (
      <AddProviderModal catalog={CATALOG} onClose={() => {}} onAdded={vi.fn()} />
    ));
    const trigger = document.querySelector<HTMLElement>(".oc-dropdown-trigger");
    expect(trigger).toBeTruthy();
    chooseDropdown(trigger!, "OpenRouter");
    const fields = [...document.querySelectorAll<HTMLInputElement>(".ai-settings-input")];
    expect(fields.some((field) => field.value === "https://openrouter.ai/api/v1")).toBe(true);
    expect(document.body.textContent).toContain("acme/large");
  });
});
