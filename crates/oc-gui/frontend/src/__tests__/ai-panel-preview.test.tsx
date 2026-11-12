/**
 * Previewing an edit: a proposal opened as a document, toggled between the base
 * and the proposal, with the changed subtrees highlighted, before accepting or
 * declining.
 *
 * The panel drives the four preview commands and keeps the bar; the editors
 * read one store flag and add a class. Nothing here re-renders a document: the
 * snapshot the backend returns is the document the store and the editors paint.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeNote, makePattern, makeSnapshot, makeTrack } from "./setup";
import { installAuditBackend, loadAuditProject, makeTimelineProject, mountSurface, resetSurface, settle } from "./ui-audit-support";
import { store } from "../stores/project";
import { previewStore } from "../stores/preview";
import { aiSession } from "../stores/aiSession";
import { aiLimits } from "../stores/aiLimits";
import { resetStats } from "../stores/aiStats";
import { i18n } from "../stores/i18n";
import { workspaceStore } from "../stores/workspace";
import { AiPanel } from "../components/AiPanel";
import { Timeline } from "../components/Timeline";
import { PianoRoll } from "../components/PianoRoll";
import type { PreviewHighlight, PreviewState } from "../stores/api";
import type { DiffResult } from "../ai/transcript";
import { previewChangeLine } from "../ai/previewChange";
import type { Proposal } from "../ai/agent";

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
      ],
    },
  ],
};

const BASE_DSL = '@project "Test Project"\n  tempo: 128\n';
const PROPOSAL_DSL = '@project "Test Project"\n  tempo: 128\n\n@pattern hook\n  C4 q mf\n';

const HIGHLIGHT: PreviewHighlight = {
  tracks: [],
  patterns: ["hook"],
  inline_patterns: [],
  presets: [],
  clips: [],
  effects: [],
  notes: [{ pattern: "hook", start_bar: 1, end_bar: 1 }],
};

const PROPOSAL: Proposal = {
  path: "pattern:hook",
  dsl: "@pattern hook\n  C4 q mf\n",
  expectedDsl: BASE_DSL,
};

const DIFF: DiffResult = {
  old_text: BASE_DSL,
  new_text: PROPOSAL_DSL,
  hunks: [{ lines: [{ kind: "Added", text: "@pattern hook" }] }],
} as DiffResult;

/** Two phrases of the same name leave one range and arrive at another. */
const MOVED_DIFF: DiffResult = {
  old_text: '@track drums | instrument: drums/acoustic_kit\n  @phrase beat [1:1 - 3:1]\n  @phrase beat [5:1 - 7:1]\n',
  new_text: '@track drums | instrument: drums/acoustic_kit\n  @phrase beat [9:1 - 11:1]\n  @phrase beat [13:1 - 15:1]\n',
  hunks: [{
    lines: [
      { kind: "Same", text: "@track drums | instrument: drums/acoustic_kit" },
      { kind: "Removed", text: "  @phrase beat [1:1 - 3:1]" },
      { kind: "Removed", text: "  @phrase beat [5:1 - 7:1]" },
      { kind: "Added", text: "  @phrase beat [9:1 - 11:1]" },
      { kind: "Added", text: "  @phrase beat [13:1 - 15:1]" },
    ],
  }],
};

/** Notes and mix level of one existing track: two named clauses. */
const NOTES_AND_MIX_DIFF: DiffResult = {
  old_text: "@track piano | instrument: piano/acoustic_grand\n  @phrase intro [1:1 - 5:1]\n    C4 q mf\n  mix: vol=-3dB\n",
  new_text: "@track piano | instrument: piano/acoustic_grand\n  @phrase intro [1:1 - 5:1]\n    D4 q mf\n  mix: vol=-6dB\n",
  hunks: [{
    lines: [
      { kind: "Same", text: "@track piano | instrument: piano/acoustic_grand" },
      { kind: "Same", text: "  @phrase intro [1:1 - 5:1]" },
      { kind: "Removed", text: "    C4 q mf" },
      { kind: "Added", text: "    D4 q mf" },
      { kind: "Removed", text: "  mix: vol=-3dB" },
      { kind: "Added", text: "  mix: vol=-6dB" },
    ],
  }],
};

/** A diff with no hunks says nothing about what changed inside the document. */
const REPLACED_DIFF: DiffResult = {
  old_text: '@project "Old"\n',
  new_text: '@project "New"\n',
  hunks: [],
};

function baseSnapshot() {
  return { ...makeSnapshot(), dsl_source: BASE_DSL };
}

function proposalSnapshot() {
  return { ...makeSnapshot(), dsl_source: PROPOSAL_DSL };
}

/** The backend the panel talks to: config, tools, and the preview commands. */
function installBackend() {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (command) {
      case "load_ai_config": return { version: 2, active_provider: "", providers: [] };
      case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      case "ai_toolsets": return [{ id: "project-edit", title: "Project edit", description: "", tools: ["tracks_list"], default_on: true }];
      case "ai_tools": return [{ name: "tracks_list", description: "List tracks", area: "project", mutation: false, inputSchema: {} }];
      case "sessions_list": return [];
      case "preview_state": return null;
      case "preview_begin":
        return {
          ...proposalSnapshot(),
          preview: { side: "proposal", target_path: "pattern:hook", highlight: HIGHLIGHT } satisfies PreviewState,
        };
      case "preview_show":
        return {
          ...(typed.side === "base" ? baseSnapshot() : proposalSnapshot()),
          preview: {
            side: typed.side === "base" ? "base" : "proposal",
            target_path: "pattern:hook",
            highlight: HIGHLIGHT,
          } satisfies PreviewState,
        };
      case "preview_accept": return { ...proposalSnapshot(), dirty: true };
      case "preview_cancel": return baseSnapshot();
      default: return baseSnapshot();
    }
  });
}

const previewButton = () => document.querySelector(".ai-preview-open") as HTMLButtonElement | null;
const previewBar = () => document.querySelector(".ai-preview-bar") as HTMLElement | null;
const applyButton = () => document.querySelector(".ai-preview-apply") as HTMLButtonElement | null;
const rewindButton = () => document.querySelector(".ai-preview-rewind") as HTMLButtonElement | null;
const acceptButton = () => document.querySelector(".ai-preview-accept") as HTMLButtonElement | null;
const declineButton = () => document.querySelector(".ai-preview-decline") as HTMLButtonElement | null;
const composer = () => document.querySelector(".ai-input") as HTMLTextAreaElement | null;

/** A panel holding one validated proposal, as a paused turn would leave it. */
async function mountWithProposal(diff: DiffResult = DIFF) {
  await store.initProject();
  aiSession.setPending({ proposal: PROPOSAL, diff });
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(previewButton()).toBeTruthy());
  return view;
}

/** The panel with the preview open, which is where the change line lives. */
async function openPreview(diff: DiffResult) {
  await mountWithProposal(diff);
  fireEvent.click(previewButton()!);
  await waitFor(() => expect(previewBar()).toBeTruthy());
}

beforeEach(() => {
  installBackend();
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  aiSession.reset();
  workspaceStore.reset();
  previewStore.reset();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  previewStore.reset();
  resetSurface();
  workspaceStore.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("previewing a proposal", () => {
  it("offers Preview edit on the proposal and opens the proposal as the document", async () => {
    await mountWithProposal();
    expect(previewButton()!.textContent).toBe(i18n.t("ai.preview.start"));

    fireEvent.click(previewButton()!);
    await waitFor(() => expect(previewBar()).toBeTruthy());

    expect(invokeMock).toHaveBeenCalledWith("preview_begin", {
      newDsl: PROPOSAL.dsl,
      targetPath: PROPOSAL.path,
      expectedDsl: PROPOSAL.expectedDsl,
    });
    // The proposal is the document the session now shows.
    expect(store.dslSource()).toBe(PROPOSAL_DSL);
    // The bar names the side on screen and hides the plain accept row.
    expect(applyButton()!.classList.contains("active")).toBe(true);
    expect(document.querySelector(".ai-accept-bar")).toBeNull();
    expect(document.querySelector(".ai-preview-hint")!.textContent).toContain("proposed version");
  });

  it("swaps to the base and back without touching the proposal", async () => {
    await mountWithProposal();
    fireEvent.click(previewButton()!);
    await waitFor(() => expect(previewBar()).toBeTruthy());

    fireEvent.click(rewindButton()!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("preview_show", { side: "base" }));
    await waitFor(() => expect(store.dslSource()).toBe(BASE_DSL));
    expect(rewindButton()!.classList.contains("active")).toBe(true);
    expect(document.querySelector(".ai-preview-hint")!.textContent).toContain("base version");

    fireEvent.click(applyButton()!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("preview_show", { side: "proposal" }));
    await waitFor(() => expect(store.dslSource()).toBe(PROPOSAL_DSL));
    expect(applyButton()!.classList.contains("active")).toBe(true);
  });

  it("accepts the previewed edit and closes the bar", async () => {
    await mountWithProposal();
    fireEvent.click(previewButton()!);
    await waitFor(() => expect(previewBar()).toBeTruthy());

    fireEvent.click(acceptButton()!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("preview_accept"));
    await waitFor(() => expect(previewBar()).toBeNull());
    expect(document.querySelector(".ai-accept-bar")).toBeNull();
    expect(store.dslSource()).toBe(PROPOSAL_DSL);
    expect(aiSession.pending()).toBeNull();
  });

  it("declines the previewed edit and restores the base document", async () => {
    await mountWithProposal();
    fireEvent.click(previewButton()!);
    await waitFor(() => expect(previewBar()).toBeTruthy());

    fireEvent.click(declineButton()!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("preview_cancel"));
    await waitFor(() => expect(previewBar()).toBeNull());
    expect(store.dslSource()).toBe(BASE_DSL);
    expect(aiSession.pending()).toBeNull();
  });

  it("disables the composer with the sentence the command layer uses", async () => {
    await mountWithProposal();
    // A pending proposal already blocks sending; the preview replaces the
    // reason with the one every refused mutation returns.
    expect(document.querySelector(".ai-send-blocked")!.textContent).toBe(i18n.t("ai.sendBlocked.reviewEdit"));

    fireEvent.click(previewButton()!);
    await waitFor(() => expect(previewBar()).toBeTruthy());

    expect(composer()!.disabled).toBe(true);
    // One sentence, shown here and returned by every refused mutation.
    expect(previewStore.open()).toBe(true);
    expect(document.querySelector(".ai-send-blocked")!.textContent).toBe(i18n.t("ai.preview.readOnly"));
    expect(i18n.t("ai.preview.readOnly")).toBe("Finish the preview before editing.");

    fireEvent.click(declineButton()!);
    await waitFor(() => expect(composer()!.disabled).toBe(false));
  });

  it("keeps the proposal when the backend refuses to open the preview", async () => {
    installBackend();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "preview_begin") {
        throw "The document changed since this proposal was made, so the preview was refused.";
      }
      if (command === "load_ai_config") return { version: 2, active_provider: "", providers: [] };
      if (command === "ai_catalog") return CATALOG;
      if (command === "ai_toolsets") return [{ id: "project-edit", title: "Project edit", description: "", tools: [], default_on: true }];
      if (command === "ai_tools") return [];
      if (command === "sessions_list") return [];
      return baseSnapshot();
    });
    await mountWithProposal();

    fireEvent.click(previewButton()!);
    await waitFor(() => expect(previewBar()).toBeNull());
    // The explanation is in the transcript, and the proposal is still waiting.
    await waitFor(() => expect(document.body.textContent).toContain("preview was refused"));
    expect(previewStore.open()).toBe(false);
    expect(aiSession.pending()).not.toBeNull();
    expect(store.dslSource()).toBe(BASE_DSL);
  });

  it("localizes the recording refusal in the edit preview", async () => {
    i18n.setLocale("zh-Hans");
    invokeMock.mockRejectedValueOnce("Stop recording before previewing an edit.");
    expect(await previewStore.begin(PROPOSAL)).toBe(false);
    expect(previewStore.error()).toBe("请先停止录制，再预览编辑。");
    for (const locale of i18n.locales) {
      expect(i18n.has(locale, "ai.preview.recording")).toBe(true);
    }
  });
});

describe("the change line", () => {
  const changeLine = () => document.querySelector(".ai-preview-change") as HTMLElement | null;

  it("names the change the proposal's diff makes, not the model's prose", async () => {
    await openPreview(MOVED_DIFF);

    // Two phrases left their ranges: the line says what the edit does.
    expect(changeLine()!.textContent).toBe("Moves 2 clips in “drums”.");
    expect(i18n.t("ai.preview.change.clipMoveMany", { count: 2, track: "drums" }))
      .toBe("Moves 2 clips in “drums”");
  });

  it("joins a notes change and a settings change with the track's phrase name", async () => {
    await openPreview(NOTES_AND_MIX_DIFF);

    expect(changeLine()!.textContent).toBe("Changes the notes of “intro” and adjusts the settings.");
  });

  it("says the document was replaced when the diff has no hunks", async () => {
    await openPreview(REPLACED_DIFF);

    expect(changeLine()!.textContent).toBe(i18n.t("ai.preview.change.wholesale"));
    expect(changeLine()!.textContent).toBe("Replaces the whole project document.");
  });

  it("keeps the line inside the bar's text column, above the side hint", async () => {
    await openPreview(MOVED_DIFF);

    const column = document.querySelector(".ai-preview-text")!;
    expect(column.contains(changeLine())).toBe(true);
    expect(column.querySelector(".ai-preview-hint")).toBeTruthy();
    // The buttons are the bar's other children, outside the wrapping column.
    expect(column.contains(applyButton())).toBe(false);
    expect(previewBar()!.querySelector(".ai-preview-sides")).toBeTruthy();
    expect(previewBar()!.querySelector(".ai-preview-actions")).toBeTruthy();
  });

  it("translates the line in all five locales", async () => {
    for (const key of i18n.keys().filter((name) => name.startsWith("ai.preview.change."))) {
      for (const locale of i18n.locales) expect(i18n.has(locale, key), `${locale} ${key}`).toBe(true);
    }
  });

  it("follows the locale on screen", async () => {
    await openPreview(MOVED_DIFF);
    expect(changeLine()!.textContent).toBe("Moves 2 clips in “drums”.");

    i18n.setLocale("zh-Hans");
    await waitFor(() => expect(changeLine()!.textContent).toBe("在“drums”中移动 2 个片段。"));
    expect(changeLine()!.textContent).not.toContain("Moves");
  });
});

describe("reading a diff as a change", () => {
  const diffOf = (lines: [string, string][], old_text = "old", new_text = "new"): DiffResult =>
    ({ old_text, new_text, hunks: [{ lines: lines.map(([kind, text]) => ({ kind, text })) }] });

  it("counts an added track, and one clip inside it, by name", () => {
    const line = previewChangeLine(diffOf([
      ["Added", "@track bass | instrument: bass/electric_finger"],
      ["Added", "  @phrase riff [1:1 - 3:1]"],
      ["Added", "    C2 q mf"],
    ], "", "@track bass | instrument: bass/electric_finger\n  @phrase riff [1:1 - 3:1]\n    C2 q mf\n"));
    // The clip's notes are part of naming the clip, so they get no clause.
    expect(line).toBe("Adds a track “bass” and adds a clip in “bass”.");
  });

  it("reads a clip header that left its range as a move", () => {
    const line = previewChangeLine(diffOf([
      ["Same", "@track drums | instrument: drums/acoustic_kit"],
      ["Removed", "  @phrase beat [1:1 - 3:1]"],
      ["Added", "  @phrase beat [5:1 - 7:1]"],
    ]));
    expect(line).toBe("Moves a clip in “drums”.");
  });

  it("reads a declaration on both sides as a change in place, not an add and a remove", () => {
    const line = previewChangeLine(diffOf([
      ["Removed", "@track piano | instrument: piano/acoustic_grand"],
      ["Added", "@track piano | instrument: piano/electronic_grand"],
    ]));
    expect(line).toBe("Adjusts the settings.");
  });

  it("reads a renamed track as an add and a remove", () => {
    const line = previewChangeLine(diffOf([
      ["Removed", "@track piano | instrument: piano/acoustic_grand"],
      ["Added", "@track keys | instrument: piano/acoustic_grand"],
    ]));
    expect(line).toBe("Adds a track “keys” and removes the track “piano”.");
  });

  it("names nothing when the diff carries no changed line", () => {
    expect(previewChangeLine(diffOf([["Same", "@project \"A\""]]))).toBeNull();
    expect(previewChangeLine(null)).toBeNull();
  });

  it("says the whole document was replaced when nothing is shared with the base", () => {
    const line = previewChangeLine(diffOf([
      ["Removed", "@project \"A\""],
      ["Added", "@project \"B\""],
      ["Removed", "@track x | instrument: piano/acoustic_grand"],
      ["Added", "@track y | instrument: piano/acoustic_grand"],
    ], "@project \"A\"\n@track x | instrument: piano/acoustic_grand\n", "@project \"B\"\n@track y | instrument: piano/acoustic_grand\n"));
    expect(line).toBe("Replaces the whole project document.");
  });

  it("names at most three kinds, in significance order", () => {
    const line = previewChangeLine(diffOf([
      ["Same", "@track bass | instrument: bass/electric_finger"],
      ["Same", "  @phrase riff [1:1 - 3:1]"],
      ["Removed", "    C2 q mf"],
      ["Added", "    D2 q mf"],
      ["Removed", "  @phrase riff [5:1 - 7:1]"],
      ["Added", "  @phrase fill [3:1 - 5:1]"],
      ["Removed", "@bus hall"],
      ["Added", "@bus room"],
    ]));
    // Four kinds in the diff: the three most significant are named.
    expect(line).toBe("Removes a clip from “bass”, adds a clip in “bass” and changes the notes of “riff”.");
  });
});

describe("preview highlighting", () => {
  it("marks the changed track and its clip in the timeline", async () => {
    const snapshot = makeTimelineProject();
    await loadAuditProject(snapshot);
    const highlight: PreviewHighlight = {
      tracks: ["Piano"],
      patterns: [],
      inline_patterns: [],
      presets: [],
      clips: [{ track: "Piano", kind: "pattern", name: "hook", base_index: 0, proposal_index: 0 }],
      effects: [],
      notes: [],
    };
    installAuditBackend((command) =>
      command === "preview_state"
        ? { side: "proposal", target_path: "track:Piano", highlight } satisfies PreviewState
        : undefined,
    );
    await previewStore.refresh();
    expect(previewStore.open()).toBe(true);

    await mountSurface(() => <Timeline />);
    const lanes = [...document.querySelectorAll(".track-lane")];
    expect(lanes).toHaveLength(2);
    // The first lane is Piano; only it carries the proposal's mark.
    expect(lanes[0].classList.contains("preview-changed")).toBe(true);
    expect(lanes[1].classList.contains("preview-changed")).toBe(false);
    expect(document.querySelectorAll(".phrase-block.preview-changed")).toHaveLength(1);
  });

  it("marks only the notes inside a changed bar of the open pattern", async () => {
    const pattern = makePattern("hook");
    pattern.hits = [];
    pattern.events = [
      makeNote({ start_beat: 0 }),
      makeNote({ start_beat: 4, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ];
    const track = makeTrack("Piano", 0);
    track.clips = [{
      pattern: "hook",
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    }];
    await loadAuditProject(makeSnapshot({ tracks: [track], patterns: [pattern] }));
    const highlight: PreviewHighlight = {
      tracks: [],
      patterns: ["hook"],
      inline_patterns: [],
      presets: [],
      clips: [],
      effects: [],
      notes: [{ pattern: "hook", start_bar: 2, end_bar: 2 }],
    };
    installAuditBackend((command) =>
      command === "preview_state"
        ? { side: "proposal", target_path: "pattern:hook", highlight } satisfies PreviewState
        : undefined,
    );
    await previewStore.refresh();

    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("hook");
    await mountSurface(() => <PianoRoll />);
    await settle();

    const marked = [...document.querySelectorAll(".pianoroll-note.preview-changed")];
    expect(marked).toHaveLength(1);
    // The note on beat 4 is the one the proposal changed; the one at beat 0 is not.
    expect(Number(marked[0].getAttribute("style")?.match(/top:\s*(\d+)/)?.[1] ?? -1)).toBeGreaterThan(0);
  });
});
