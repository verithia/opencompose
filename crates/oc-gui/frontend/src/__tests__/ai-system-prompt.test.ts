import { describe, expect, it } from "vitest";
import {
  buildSystemPrompt,
  noteTokens,
  PROJECT_NOTE_MAX_CHARS,
  PROJECT_NOTE_TOKEN_BUDGET,
  type SkillRef,
} from "../ai/system-prompt";
import type { ToolDef } from "../ai/agent";
import type { ModelCapabilities } from "../ai/config";

const capabilities: ModelCapabilities = {
  text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true,
};
const def = (name: string, area: string, properties: Record<string, any> = {}, required: string[] = []): ToolDef =>
  ({name, description:name, area, inputSchema:{type:"object",properties,required,additionalProperties:false}});
const ctx = {
  projectName: "Test", projectDsl: "@project", tempo: 120, timeSig: "4/4", key: "C major",
  trackCount: 1, busCount: 0,
};
const tools = (): ToolDef[] => [
  def("project_extract", "project", {path:{type:"string"}}, ["path"]),
  def("render_range", "render", {path:{type:"string"},start:{type:"number"},end:{type:"number"},stems:{type:"boolean"}}, ["path","start","end"]),
  def("audition_range", "render", {path:{type:"string"},start:{type:"number"},end:{type:"number"},tracks:{type:"array",items:{type:"integer"}}}),
  def("audio_analyze", "analysis", {path:{type:"string"}}, ["path"]),
  def("analyze_notes", "analysis", {path:{type:"string"},start:{type:"number"},end:{type:"number"},min_confidence:{type:"number"},max_notes:{type:"integer"},tempo:{type:"number"},time_signature:{type:"string"},quantize:{type:"boolean"},project:{type:"string"}}, ["path"]),
  def("separate_stems", "analysis", {path:{type:"string"},mode:{type:"string"},out_dir:{type:"string"}}, ["path"]),
  def("audio_compare", "analysis", {reference:{type:"string"},candidate:{type:"string"}}, ["reference","candidate"]),
  def("transaction_begin", "transaction", {label:{type:"string"}}),
  def("transaction_rollback", "transaction"),
  def("edit_subtree", "project", {path:{type:"string"},new_dsl:{type:"string"}}, ["path","new_dsl"]),
];

describe("panel system prompt", () => {
  it("tells an audio-capable model it can hear, and says nothing about ears otherwise", () => {
    const hearing = { ...capabilities, audioIn: true };
    const heard = buildSystemPrompt(ctx, hearing, tools());
    expect(heard).toContain("## Listening");
    expect(heard).toContain("audition_range");

    // a model that cannot receive audio is not told to listen
    expect(buildSystemPrompt(ctx, capabilities, tools())).not.toContain("## Listening");
    // and the section never appears when the tool is not offered
    const withoutAudition = tools().filter((tool) => tool.name !== "audition_range");
    expect(buildSystemPrompt(ctx, hearing, withoutAudition)).not.toContain("## Listening");
  });

  it("teaches the comma form for notes at one position", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, tools());
    expect(prompt).toContain("comma list");
    expect(prompt).toContain("@0.0 G4 q mf, G5 q mf");
    expect(prompt).toContain("[D4 F4 A4] h mp");
  });

  it("summarises the advertised registry by area instead of copying schemas", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, tools());
    expect(prompt).toContain("### Project");
    expect(prompt).toContain("### Range render");
    expect(prompt).toContain("### Analysis");
    expect(prompt).toContain("### Transactions");
    expect(prompt).toContain("render_range(path, start, end, stems?)");
    expect(prompt).toContain("audition_range(path?, start?, end?, tracks?)");
    expect(prompt).toContain("16 kHz mono clip");
    expect(prompt).toContain("analyze_notes(path, start?, end?, min_confidence?, max_notes?, tempo?, time_signature?, quantize?, project?)");
    expect(prompt).toContain("one monophonic detector");
    expect(prompt).toContain("polyphonic=true");
    expect(prompt).toContain("separate_stems(path, mode?, out_dir?)");
    expect(prompt).toContain("its `method` says which ran");
    expect(prompt).toContain("not a source separation");
    expect(prompt).toContain("audio_compare(reference, candidate)");
    expect(prompt).toContain("transaction_begin(label?)");
    expect(prompt).toContain("edit_subtree(path, new_dsl)");
    expect(prompt).toContain("expected_revision");
    expect(prompt).toContain("### Not available here");
    expect(prompt).toContain("edit_subtree` proposes a replacement");
    expect(prompt).not.toContain("render_offline(");
    expect(prompt).not.toContain("plugins_scan(");
  });
  it("states the approval rule only when a destructive tool is offered", () => {
    const deleting = [...tools(), {...def("tracks_remove", "tracks", {index:{type:"integer"}}, ["index"]), approval:"confirm" as const}];
    const prompt = buildSystemPrompt(ctx, capabilities, deleting);
    expect(prompt).toContain("## Destructive calls");
    expect(prompt).toContain("`tracks_remove`");
    expect(prompt).toContain("A person approves one before it runs");
    expect(prompt).toContain("a session with nobody to ask refuses it");

    // A surface without a destructive tool has nothing to warn about.
    expect(buildSystemPrompt(ctx, capabilities, tools())).not.toContain("## Destructive calls");
  });

  it("switches the tool-call format with native tool support", () => {
    expect(buildSystemPrompt(ctx, capabilities, tools())).toContain("native function tools");
    const text = buildSystemPrompt(ctx, {...capabilities, nativeToolCalling:false}, tools());
    expect(text).toContain('{"tool":"NAME","args":{...}}');
  });

  const skillTools = (): ToolDef[] => [
    def("list_skills", "skills"),
    def("read_skill", "skills", {id:{type:"string"}}, ["id"]),
    ...tools(),
  ];
  const skillIndex: SkillRef[] = [
    {id:"dsl-subtree-paths", title:"DSL subtree paths", category:"editing"},
    {id:"writing-notes", title:"Writing notes and phrases", category:"editing"},
  ];

  it("includes the skill index only when the skill tools are offered", () => {
    const offered = buildSystemPrompt(ctx, capabilities, skillTools(), skillIndex);
    expect(offered).toContain("## Skills");
    expect(offered).toContain("dsl-subtree-paths");
    expect(offered).toContain("guess at DSL syntax");
    expect(offered).toContain("do not recognise");
    // One sentence: a refusal may name a skill, and reading it is the retry.
    expect(offered).toContain("A refused tool call may carry a `skill` id; read that skill before retrying.");

    const absent = buildSystemPrompt(ctx, capabilities, tools());
    expect(absent).not.toContain("## Skills");
    expect(absent).not.toContain("list_skills");
    expect(absent).not.toContain("may carry a `skill` id");

    // Half the pair is not enough to act on the index.
    const half = buildSystemPrompt(ctx, capabilities, [def("list_skills", "skills")]);
    expect(half).not.toContain("## Skills");
  });

  it("orders tool docs before the skill index and the project DSL last", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, skillTools(), skillIndex);
    const toolDocs = prompt.indexOf("### Project");
    const skills = prompt.indexOf("## Skills");
    const projectDsl = prompt.indexOf("## Current project DSL");
    expect(toolDocs).toBeGreaterThan(-1);
    expect(skills).toBeGreaterThan(toolDocs);
    expect(projectDsl).toBeGreaterThan(skills);
  });

  it("never carries a skill body into the prompt", () => {
    const withBodies = [{...skillIndex[0], body:"SECRET-SKILL-BODY"}] as unknown as SkillRef[];
    const prompt = buildSystemPrompt(ctx, capabilities, skillTools(), withBodies);
    expect(prompt).not.toContain("SECRET-SKILL-BODY");
  });

  /** A note at the store's cap, spread over `lines` roughly equal lines. */
  const maxNote = (lines = 1): string => {
    const width = Math.floor((PROJECT_NOTE_MAX_CHARS - (lines - 1)) / lines);
    return Array.from({ length: lines }, (_, index) =>
      (`Stem ${index + 1} prints at -14 LUFS. `).padEnd(width, "x").slice(0, width)).join("\n");
  };

  it("carries this project's note once, and never another project's", () => {
    const a = { ...ctx, projectNote: "Always print stems at -14 LUFS." };
    const b = { ...ctx, projectNote: "This song is in D minor; do not modulate." };
    const promptA = buildSystemPrompt(a, capabilities, panelTools(), skillIndex);
    const promptB = buildSystemPrompt(b, capabilities, panelTools(), skillIndex);
    // Exactly once: the note is data, so no other section repeats it.
    expect(promptA.split("Always print stems at -14 LUFS.").length - 1).toBe(1);
    expect(promptA).not.toContain("do not modulate");
    expect(promptB.split("This song is in D minor; do not modulate.").length - 1).toBe(1);
    expect(promptB).not.toContain("-14 LUFS");
    // Placement: after the base prompt, before the first tool material.
    const heading = promptA.indexOf("## Project instructions");
    expect(heading).toBeGreaterThan(promptA.indexOf("## Behavior rules"));
    expect(heading).toBeLessThan(promptA.indexOf("## Available tools"));
  });

  it("adds nothing at all when the project has no note", () => {
    const bare = buildSystemPrompt(ctx, capabilities, panelTools(), skillIndex);
    expect(bare).not.toContain("## Project instructions");
    for (const projectNote of ["", "   ", "\n\t \n", undefined]) {
      expect(buildSystemPrompt({ ...ctx, projectNote }, capabilities, panelTools(), skillIndex)).toBe(bare);
    }
  });

  const panelTools = (): ToolDef[] => [
    ...skillTools(),
    def("presets_list", "presets"),
    def("presets_set_params", "presets", {name:{type:"string"},params:{type:"object"}}, ["name","params"]),
    def("presets_set_param", "presets", {name:{type:"string"},param:{type:"string"},value:{type:"string"}}, ["name","param"]),
    def("devices_catalog", "devices"),
    def("plugins_programs", "plugin", {preset:{type:"string"}}, ["preset"]),
    def("plugins_select_program", "plugin", {preset:{type:"string"},index:{type:"integer"}}, ["preset"]),
    def("content_libraries", "plugin"),
    def("content_library_register", "plugin", {path:{type:"string"}}, ["path"]),
  ];

  it("teaches the capabilities the DSL supports beyond the basic reference", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, panelTools());
    for (const section of [
      "### Automation lanes",
      "### Sections and repeat",
      "### Drum kits (@kit)",
      "### Placed audio (@audio_clip)",
    ]) {
      expect(prompt, section).toContain(section);
    }
    // automation: two targets, two points, a curve and the lane attributes.
    // A control lane is a step list, so `curve=`/`tension=` belong to the
    // effect-parameter lane only.
    expect(prompt).toContain("automation: brightness {1:1=20%, 5:1=90%}");
    expect(prompt).toContain("automation: eq.high_gain [ppq=480] smoothing=25ms");
    // a section repeat and the kit/audio-clip forms the prompt never taught
    expect(prompt).toContain("repeat: 2");
    // the section reference names the tools that write a repeat, not just the syntax
    expect(prompt).toContain("sections_list");
    expect(prompt).toContain("sections_add");
    expect(prompt).toContain("sections_remove");
    expect(prompt).toContain("@kit basic");
    expect(prompt).toContain("pad kick  | note: C2");
    expect(prompt).toContain("@audio_clip intro [1:1 - 5:1] | track: tape");
    expect(prompt).toContain('file: "media/intro.flac"');
  });

  it("keeps every reference section the prompt already taught", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, tools());
    for (const title of [
      "Project header (tempo and meter changes)",
      "Presets (sound sources)",
      "Named patterns (reusable content)",
      "Track with effects, mix and a clip",
      "Inline phrases (notes directly in tracks)",
      "Several notes at once (one position, comma list)",
      "Drum notation",
      "Buses (shared routing)",
      "Master",
      "Sections and repeat",
      "Sustain pedal",
    ]) {
      expect(prompt, title).toContain(`### ${title}`);
    }
    for (const prose of ["### Pitch:", "### Duration", "### Dynamics", "### Articulations", "### Key rules"]) {
      expect(prompt, prose).toContain(prose);
    }
    // the core note forms render verbatim
    expect(prompt).toContain("@0.0 G4 q mf, G5 q mf");
    expect(prompt).toContain("@0 pedal down");
    expect(prompt).toContain("@2.0 pedal down");
    expect(prompt).toContain("[D4 F4 A4] h mp");
    expect(prompt).toContain("mix: vol=-3dB, pan=0, send(reverb=30%)");
    expect(prompt).toContain("Built-in effects: eq, passive_eq");
    expect(prompt).toContain("Drums: kick, snare, hat_closed");
  });

  it("tells the model to read names before setting them", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, panelTools());
    expect(prompt).toContain("## Finding names");
    expect(prompt).toContain("presets_list");
    expect(prompt).toContain("devices_catalog");
    expect(prompt).toContain("presets_set_params");
    expect(prompt).toContain("plugins_programs");
    expect(prompt).toContain("content_library_register");
    // names the panel cannot enumerate are not advertised
    expect(buildSystemPrompt(ctx, capabilities, tools())).not.toContain("## Finding names");
  });

  it("states the working loop only when there is a way to check the result", () => {
    const prompt = buildSystemPrompt(ctx, capabilities, panelTools());
    expect(prompt).toContain("## How to work");
    expect(prompt).toContain("before editing");
    const noMeasure = tools().filter((tool) => !["render_range", "audio_analyze", "audition_range"].includes(tool.name));
    expect(buildSystemPrompt(ctx, capabilities, noMeasure)).not.toContain("## How to work");
  });

  it("keeps the whole prompt within its line and token budget", () => {
    const prompt = buildSystemPrompt(ctx, {...capabilities, audioIn: true}, panelTools(), skillIndex);
    const lines = prompt.split("\n").length;
    const tokens = Math.ceil(prompt.length / 4);
    // 2026-10-29: 250 lines / 2935 chars-4 tokens after the reference became
    // parsed data (219 / 2210 before) and six producer tools joined the panel
    // surface. Tool docs are generated from the registry, so their signatures
    // are the cost of advertising them; the ceiling moved with the surface
    // (2800 -> 2900 -> 3000) rather than trimming teaching text. Today it sits at
    // 2935: plugin state tools, the beat-grid note, the producer tools and the
    // section reference. The sustain-pedal reference cost a section and a key
    // rule (3069 / 262 lines), so the ceiling moved with it. The glissando
    // reference costs one section (3193 / 271 lines). `swing:` and
    // `gain_automation:` were missing from the reference entirely, and the
    // former cost a measured session five rounds, so they are taught here
    // too. The hairpin section is one line plus the notes it spans. The
    // approval paragraph plus the tool doc that advertises a removal costs a
    // section, so the ceiling is measured with a destructive tool present
    // (280 lines / 3400 tokens); a surface without one stays below it.
    expect(lines, `${lines} lines`).toBeLessThanOrEqual(280);
    expect(tokens, `${tokens} chars/4`).toBeLessThanOrEqual(3400);
    // Worst case: a surface that also offers a destructive tool pays for the
    // approval section, so the budget is measured with it present.
    const deleting = [...panelTools(), {...def("tracks_remove", "tracks", {index:{type:"integer"}}, ["index"]), approval:"confirm" as const}];
    const withApproval = buildSystemPrompt(ctx, {...capabilities, audioIn: true}, deleting, skillIndex);
    expect(withApproval.split("\n").length, `${withApproval.split("\n").length} lines with approval`).toBeLessThanOrEqual(280);
    expect(Math.ceil(withApproval.length / 4), `${Math.ceil(withApproval.length / 4)} chars/4 with approval`).toBeLessThanOrEqual(3400);
    // The project note is counted against the same budget. A note at the
    // store's cap costs its own share, PROJECT_NOTE_TOKEN_BUDGET, plus the
    // heading and the one sentence that introduces it: measured 279 lines /
    // 3383 tokens without a note, 304 lines / 4422 tokens with a full 20-line
    // note. The ceilings below are the base budget plus that share.
    const note = maxNote(20);
    expect(note.length).toBeLessThanOrEqual(PROJECT_NOTE_MAX_CHARS);
    expect(noteTokens(note)).toBeLessThanOrEqual(PROJECT_NOTE_TOKEN_BUDGET);
    const withNote = buildSystemPrompt(
      {...ctx, projectNote: note}, {...capabilities, audioIn: true}, deleting, skillIndex,
    );
    const noteLines = withNote.split("\n").length;
    const noteTokensUsed = Math.ceil(withNote.length / 4);
    expect(noteLines, `${noteLines} lines with a full note`).toBeLessThanOrEqual(280 + 20 + 8);
    expect(noteTokensUsed, `${noteTokensUsed} chars/4 with a full note`)
      .toBeLessThanOrEqual(3400 + PROJECT_NOTE_TOKEN_BUDGET + 60);
  });
});
