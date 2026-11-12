import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { AiThinking } from "../components/AiThinking";
import { activitySummary, preparingActivity, preparingToolFromText, appendThinking, THINKING_DISPLAY_LIMIT, thinkingPreview } from "../ai/activity";
import { i18n } from "../stores/i18n";

describe("concise AI activity", () => {
  afterEach(() => i18n.setLocale("en"));
  it.each(["zh-Hans", "zh-Hant", "ja", "es"] as const)("localizes activity and unknown-tool fallback in %s", locale => {
    const names = ["sections_list", "sections_add", "transaction_begin", "transaction_commit", "transaction_rollback", "presets_add", "tracks_preset", "audition_range", "render_range", "devices_catalog", "presets_list", "plugins_list"];
    i18n.setLocale("en");
    const english = names.map(name => activitySummary({ name }));
    i18n.setLocale(locale);
    names.forEach((name, index) => {
      const label = activitySummary({ name });
      expect(label).not.toBe(english[index]);
      expect(label).not.toContain(name);
      expect(label).not.toContain("ai.activity.");
    });
    expect(activitySummary({ name: "future_catalog_tool" })).toBe(i18n.t("ai.activity.run", { name: "future_catalog_tool" }));
    expect(activitySummary({ name: "future_catalog_tool" })).not.toBe("Run tool: future_catalog_tool");
    expect(i18n.t("ai.header.providerSettings")).not.toBe("Provider settings");
    expect(i18n.t("ai.role.system")).not.toBe("System");
  });
  it("names reads and catalog searches without argument/result JSON", () => {
    i18n.setLocale("en");
    expect(activitySummary({name:"read_skill",args:'{"id":"playable-instruments"}',result:'{"body":"long"}'})).toBe("playable instruments");
    expect(activitySummary({name:"plugin_search",args:'{"query":"piano instruments","limit":25}'})).toBe("Search piano instruments");
    expect(activitySummary({name:"list_skills",args:"{"})).toBe("Browse skills");
    expect(activitySummary({name:"project_get",args:"{}"})).toBe("Inspect project");
  });
  it("names the bypassed or enabled insert in a localized activity row", () => {
    const tool = { name: "effects_set_bypass", args: '{"target":"track:0","index":0,"bypass":true}' };
    const receipt = '{"effect":"reverb","revision":2}';
    i18n.setLocale("en");
    expect(activitySummary(tool, receipt)).toBe("Bypass reverb #1");
    expect(activitySummary({ ...tool, args: '{"target":"track:0","index":0,"bypass":false}' }, receipt))
      .toBe("Enable reverb #1");
    i18n.setLocale("zh-Hans");
    expect(activitySummary(tool)).toBe("旁通第 1 个插槽的效果");
  });
  it("names sample results and the arrangement window", () => {
    i18n.setLocale("en");
    const sample = JSON.stringify({ count: 7, pages: [{ count: 3 }, { count: 4 }, { count: 0 }] });
    expect(activitySummary({ name: "sample_search", args: '{"query":"snare"}' }, sample))
      .toBe("7 results from 2 sites");
    expect(activitySummary({ name: "sample_search", args: '{"query":"snare"}' },
      JSON.stringify({ count: 1, pages: [{ count: 1 }] }))).toBe("1 result from 1 site");
    const snapshot = JSON.stringify({ project: { sections: [
      { time_range: { start: { bar: 1, beat: 1 }, end: { bar: 9, beat: 1 } } },
      { time_range: { start: { bar: 9, beat: 1 }, end: { bar: 17, beat: 1 } } },
    ] } });
    expect(activitySummary({ name: "project_get", args: "{}" }, snapshot))
      .toBe("Inspect project bars 1–16");
    expect(activitySummary({ name: "sample_pull", args: '{"id":"ia:snare"}' }))
      .toBe("Pull “ia:snare”");
    expect(activitySummary({ name: "analyze_notes", args: '{"path":"/tmp/a.wav","start":0,"end":4}' },
      JSON.stringify({ start_s: 0, end_s: 4, notes: Array.from({ length: 12 }, (_, index) => ({ midi: 60 + index })) })))
      .toBe("Analyze notes, 0s – 4s into 12 notes");
  });
  it("names the object and the bars a mutation touches", () => {
    i18n.setLocale("en");
    expect(activitySummary({ name: "tracks_add", args: '{"name":"Keys"}' }))
      .toBe("Add track “Keys”");
    expect(activitySummary({ name: "tracks_rename", args: '{"index":2,"name":"Lead"}' }))
      .toBe("Rename track #2 to “Lead”");
    expect(activitySummary({ name: "sections_add", args: '{"name":"verse","start":"1:1","end":"17:1"}' }))
      .toBe("Add section “verse” bars 1–16");
    expect(activitySummary({
      name: "project_edit",
      args: JSON.stringify({ path: "tracks/0", dsl: '@track keys | instrument: synth/lead\n@phrase hook [1:1 - 5:1]\n  @0.0 C5 q f\n' }),
    })).toBe("Edit “keys”: phrase “hook” bars 1–4");
    expect(activitySummary({
      name: "project_edit",
      args: JSON.stringify({ path: "tracks/0", dsl: '@track keys | instrument: synth/lead\n  clip: hook [9:1 - 17:1]\n' }),
    })).toBe("Edit “keys”: clip “hook” bars 9–16");
    expect(activitySummary({ name: "render_range", args: '{"start_bar":3,"end_bar":5,"path":"x.wav"}' }))
      .toBe("Render bars 3–4");
    expect(activitySummary({
      name: "project_edit",
      args: JSON.stringify({ path: "track:keys", dsl: '@track keys | instrument: synth/lead\n  clip: hook [3:1 - 4:1]\n' }),
    })).toBe("Edit “keys”: clip “hook” bar 3");
    expect(activitySummary({
      name: "project_edit",
      args: JSON.stringify({ path: "track:keys", dsl: '@track keys | instrument: synth/lead\n  clip: hook [3:2 - 3:5]\n' }),
    })).toBe("Edit “keys”: clip “hook” bar 3 beats 2–4");
    expect(activitySummary({
      name: "project_edit",
      args: JSON.stringify({ path: "track:keys", dsl: '@track keys | instrument: synth/lead\n  clip: hook [3:2 - 3:3]\n' }),
    })).toBe("Edit “keys”: clip “hook” bar 3 beat 2");
    expect(activitySummary({
      name: "project_edit",
      args: JSON.stringify({ path: "track:keys", dsl: '@track keys | instrument: synth/lead\n  clip: hook [3:4 - 4:2]\n' }),
    })).toBe("Edit “keys”: clip “hook” bar 3 beat 4 – bar 4 beat 2");
    i18n.setLocale("zh-Hans");
    expect(activitySummary({ name: "tracks_add", args: '{"name":"Keys"}' }))
      .toBe("添加轨道“Keys”");
  });

  it("exposes only supplied reasoning, collapsed with a short preview", () => {
    i18n.setLocale("en");
    const full = "Choose a gentle piano. ".repeat(30);
    const view = render(() => <AiThinking loading partial="" approval={false} lastTool={null} identity="provider/model" reasoning={full} />);
    const details = view.container.querySelector("details")!;
    expect(details.open).toBe(false);
    expect(view.container.querySelector(".ai-thinking-preview")?.textContent).toBe(thinkingPreview(full));
    expect(view.container.querySelector(".ai-thinking-content")?.textContent).toContain("Choose a gentle piano.");
    fireEvent.click(details.querySelector("summary")!);
    expect(details.open).toBe(true);
    expect(view.container.querySelectorAll(".ai-thinking-dot")).toHaveLength(3);
  });

  it("does not invent expandable reasoning when none was supplied", () => {
    const view = render(() => <AiThinking loading partial="" approval={false} lastTool={null} identity="provider/model" />);
    expect(view.container.querySelector("details")).toBeNull();
  });

  it("keeps completed reasoning expandable without an animated waiting indicator", async () => {
    const timer = vi.spyOn(globalThis, "setInterval");
    const originalScroll = HTMLElement.prototype.scrollIntoView;
    const scroll = vi.fn();
    HTMLElement.prototype.scrollIntoView = scroll;
    const view = render(() => <AiThinking loading={false} partial="" approval={false} lastTool={null} identity="" reasoning="Read the actual notes." />);
    expect(view.container.querySelector("details")).not.toBeNull();
    expect(view.container.querySelector(".ai-thinking-mark")).toBeNull();
    expect(view.container.querySelector('[role="status"]')).toBeNull();
    await Promise.resolve();
    expect(timer).not.toHaveBeenCalled();
    expect(scroll).not.toHaveBeenCalled();
    timer.mockRestore();
    HTMLElement.prototype.scrollIntoView = originalScroll;
  });

  it("bounds long provider text with an explicit display notice", () => {
    i18n.setLocale("en");
    const text = appendThinking("x".repeat(THINKING_DISPLAY_LIMIT - 2), "yyyyyy");
    expect(text).toContain("Display shortened");
    expect(text.length).toBeLessThan(THINKING_DISPLAY_LIMIT + 100);
    expect(appendThinking(text,"zzzz")).toBe(text);
  });
});

describe("tool preparation activity", () => {
  afterEach(() => i18n.setLocale("en"));
  it.each(["en", "zh-Hans", "zh-Hant", "ja", "es"] as const)("translates preparation in %s", locale => {
    i18n.setLocale(locale);
    for (const tool of ["edit_subtree", "sample_search", "plugin_search", "read_skill", "render_range", "unknown_tool"]) {
      expect(preparingActivity(tool)).not.toContain("ai.preparing.");
    }
    const view = render(() => <AiThinking loading partial="" approval={false} lastTool={null} identity="test" preparingTool="sample_search" />);
    expect(view.container.querySelector(".ai-thinking")?.getAttribute("data-phase")).toBe("preparing");
    expect(view.container.textContent).toContain(preparingActivity("sample_search"));
  });
  it("recognizes leading text-protocol calls before arguments are complete", () => {
    expect(preparingToolFromText('{"tool":"edit_subtree","args":{"dsl":"')).toBe("edit_subtree");
    expect(preparingToolFromText('```json\n{"tool":"sample_search",')).toBe("sample_search");
    expect(preparingToolFromText('Example: {"tool":"sample_search"}')).toBeNull();
    expect(preparingToolFromText('{"tool":"sample_')).toBeNull();
  });
  it("changes the status while argument tokens are still arriving", () => {
    i18n.setLocale("en");
    expect(preparingActivity("edit_subtree")).toBe("Preparing project edit");
    expect(preparingActivity("sample_search")).toBe("Preparing sample search");
    expect(preparingActivity("functions.sample_search")).toBe("Preparing sample search");
  });
});
