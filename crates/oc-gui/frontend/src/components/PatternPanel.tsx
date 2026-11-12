import { createMemo, createSignal, createUniqueId, For, Show } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { instrumentDisplay, INJECTED_PRESET_PREFIX } from "../types/project";
import type { PatternDef } from "../types/project";

import { uniqueName } from "../utils/naming";
import { patternLengthBars } from "../utils/clipContent";

import { useTooltip } from "./Tooltip";
import { openContextMenu } from "./ContextMenu";
import { TextPromptDialog } from "./TextPromptDialog";

/** The preset the dropdown shows: "" when the pattern follows its track. */
export function patternPresetValue(p: PatternDef): string {
  const name = p.preset ?? "";
  return name.startsWith(INJECTED_PRESET_PREFIX) ? "" : name;
}

function patternColorDef(p: PatternDef, index: number): string {
  return p.color ?? `var(--track-color-${index % 8})`;
}

function formatTranspose(t: number): string {
  if (t > 0) return `+${t}`;
  return `${t}`;
}

const COLOR_PRESETS = [
  "#ff6b6b", "#ff9f43", "#feca57", "#54a0ff",
  "#5f27cd", "#01a3a4", "#10ac84", "#ee5a24",
  "#6ab04c", "#e056a0", "#7e7e7e", "#222f3e",
];

export function PatternPanel() {
  const [filterText, setFilterText] = createSignal("");
  /** The pattern being renamed, by its current name. */
  const [renamePrompt, setRenamePrompt] = createSignal<string | null>(null);
  const [showAddForm, setShowAddForm] = createSignal(false);
  const [newName, setNewName] = createSignal("");
  const [newCategory, setNewCategory] = createSignal("piano");
  const [pendingPresets, setPendingPresets] = createSignal<string[]>([]);
  const [presetFailure, setPresetFailure] = createSignal<{ pattern: string; message: string } | null>(null);
  /** A length change the panel is asking about: keep the notes, trim them, or cancel. */
  const [lengthPrompt, setLengthPrompt] = createSignal<{ pattern: string; bars: number } | null>(null);
  const [lengthFailure, setLengthFailure] = createSignal<string | null>(null);
  const swingHelpId = createUniqueId();

  const presetPending = (name: string) => pendingPresets().includes(name);

  /** Write the pattern's preset and keep the row honest about the outcome. */
  const submitPreset = async (
    pattern: PatternDef,
    select: HTMLSelectElement,
    value: string | null,
  ) => {
    setPendingPresets((names) => [...names, pattern.name]);
    setPresetFailure(null);
    const failure = await store.setPatternPreset(pattern.name, value);
    setPendingPresets((names) => names.filter((name) => name !== pattern.name));
    if (failure) {
      // The write did not land. Put the row back on the project's value so the
      // dropdown cannot keep advertising a preset the pattern does not have.
      select.value = patternPresetValue(pattern);
      setPresetFailure({
        pattern: pattern.name,
        message: i18n.t("pattern.presetFailed", { message: failure }),
      });
    }
  };

  /** The pattern the editor shows. The panel reads it several times per paint,
   *  so it is cached against the pick and the pattern list. */
  const selectedP = createMemo(() => {
    const name = store.selectedPatternName();
    if (!name) return null;
    return store.project.patterns.find(p => p.name === name) ?? null;
  });

  const patterns = () => {
    const f = filterText().toLowerCase();
    const all = store.project.patterns;
    if (!f) return all;
    return all.filter(
      (p) =>
        p.name.toLowerCase().includes(f) ||
        instrumentDisplay(p.instrument).toLowerCase().includes(f),
    );
  };

  const handlePatternClick = (name: string) => {
    store.setSelectedPatternName(name);
    store.setSelectedTrackIndex(null);
  };

  const handlePatternDragStart = (e: DragEvent, name: string) => {
    e.dataTransfer?.setData("application/x-pattern", name);
    e.dataTransfer!.effectAllowed = "copy";
  };

  return (
    <div class="pattern-panel" style={{ display: "flex", "flex-direction": "column", height: "100%", background: "var(--bg-primary)" }}>
      <div class="pattern-panel-header" style={{ padding: "8px 12px", "border-bottom": "1px solid var(--border)" }}>
        <div style={{ display: "flex", "align-items": "center", gap: "8px" }}>
          <span style={{ "font-family": "var(--font-display)", "font-size": "var(--font-size-md)", "font-weight": "500", "letter-spacing": "-0.2px" }}>
            {i18n.t("panel.patterns")}
          </span>
          <span style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)" }}>
            {store.project.patterns.length}
          </span>
          <div style={{ flex: "1" }} />
          <button
            class="toolbar-btn"
            aria-label={i18n.t("pattern.new")}
            onClick={() => setShowAddForm(!showAddForm())}
            {...useTooltip(() => i18n.t("pattern.new"))}
          >+</button>
        </div>
        {showAddForm() && (
          <div style={{ "margin-top": "6px", display: "flex", "flex-wrap": "wrap", gap: "4px", "align-items": "center" }}>
            <input
              type="text" placeholder={i18n.t("pattern.namePlaceholder")} value={newName()} onInput={(e) => setNewName(e.currentTarget.value)}
              style={{ width: "80px", background: "var(--bg-input)", color: "var(--text-primary)", border: "1px solid var(--border)", "border-radius": "3px", padding: "2px 6px","font-size": "11px" }}
            />
            <select value={newCategory()} onChange={(e) => setNewCategory(e.currentTarget.value)}
              style={{ background: "var(--bg-input)", color: "var(--text-primary)", border: "1px solid var(--border)", "border-radius": "3px", padding: "2px 4px","font-size": "11px" }}
            >
              <option value="piano">{i18n.t("pattern.category.piano")}</option>
              <option value="drums">{i18n.t("store.category.drums")}</option>
              <option value="bass">{i18n.t("pattern.category.bass")}</option>
              <option value="guitar">{i18n.t("pattern.category.guitar")}</option>
              <option value="brass">{i18n.t("pattern.category.brass")}</option>
              <option value="strings">{i18n.t("pattern.category.strings")}</option>
              <option value="synth">{i18n.t("store.category.synth")}</option>
            </select>
            <button
              class="toolbar-btn"
              style={{"font-size": "11px" }}
              onClick={() => {
                const n = newName().trim();
                if (n) {
                  // Typing an existing name derives "name 2" instead of failing.
                  const unique = uniqueName(n, store.project.patterns.map((p) => p.name));
                  store.addPattern(unique, newCategory(), newCategory());
                  setNewName(""); setShowAddForm(false);
                }
              }}
            >{i18n.t("pattern.create")}</button>
            <button class="toolbar-btn" style={{"font-size": "11px" }} onClick={() => setShowAddForm(false)}>{i18n.t("dialog.cancel")}</button>
          </div>
        )}
        <input
          type="text"
          placeholder={i18n.t("pattern.filterPlaceholder")}
          value={filterText()}
          onInput={(e) => setFilterText(e.currentTarget.value)}
          style={{
            width: "100%", "margin-top": "6px",
            background: "var(--bg-input)", color: "var(--text-primary)",
            border: "1px solid var(--border)", "border-radius": "4px",
            padding: "4px 8px", "font-size": "var(--font-size-xs)",
          }}
        />
      </div>

      <div
        class="pattern-list"
        style={{
          flex: "1", overflow: "auto", padding: "8px",
        }}
      >
        {patterns().length === 0 ? (
          <div class="empty-state" style={{ padding: "40px 0" }}>
            <div class="empty-state-text">
              {filterText() ? i18n.t("pattern.noMatches") : i18n.t("pattern.noPatterns")}
            </div>
            <div class="empty-state-hint">
              {filterText()
                ? i18n.t("pattern.noMatchesHint")
                : i18n.t("pattern.noPatternsHint")}
            </div>
          </div>
        ) : (
          <For each={patterns()}>
            {(p, i) => {
              const isSelected = store.selectedPatternName() === p.name;
              const noteCount = p.events.length;
              const hitCount = p.hits.length;
              const inst = instrumentDisplay(p.instrument) || i18n.t("pattern.noInstrument");
              return (
                <div
                  class="pattern-item"
                  classList={{ selected: isSelected }}
                  style={{
                    display: "flex", "align-items": "center", gap: "8px",
                    padding: "8px 10px", "border-radius": "6px",
                    cursor: "pointer",
                    margin: "0 0 4px 0",
                    border: isSelected
                      ? "2px solid var(--accent)"
                      : "1px solid transparent",
                    background: isSelected
                      ? "var(--bg-hover)"
                      : "transparent",
                    transition: "background 0.15s",
                  }}
                  onClick={() => handlePatternClick(p.name)}
                  onDblClick={() => handlePatternClick(p.name)}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    handlePatternClick(p.name);
                    openContextMenu({
                      x: event.clientX,
                      y: event.clientY,
                      items: [
                        { type: "heading", label: p.name },
                        {
                          label: i18n.t("pattern.renameTitle"),
                          onSelect: () => { setRenamePrompt(p.name); },
                        },
                        { type: "separator" },
                        {
                          label: i18n.t("pattern.delete"),
                          danger: true,
                          onSelect: () => void store.removePattern(p.name),
                        },
                      ],
                    });
                  }}
                  draggable="true"
                  onDragStart={(e) => handlePatternDragStart(e, p.name)}
                  {...useTooltip(() => i18n.t("pattern.tooltip", {
                    name: p.name,
                    instrument: inst,
                    transpose: p.transpose != null ? ` [${formatTranspose(p.transpose)}]` : "",
                    notes: noteCount,
                    hits: hitCount,
                  }))}
                >
                  {/* Color indicator */}
                  <div
                    style={{
                      width: "4px", height: "32px", "border-radius": "2px",
                      "flex-shrink": "0",
                      background: patternColorDef(p, i()),
                    }}
                  />
                  {/* Info */}
                  <div style={{ flex: "1", "min-width": "0" }}>
                    <div style={{ "font-size": "var(--font-size-sm)", "font-weight": "500", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                      {p.name}
                    </div>
                    <div style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)" }}>
                      {inst}
                      {p.preset ? ` \u{2022} ${p.preset}` : ""}
                      {p.transpose != null ? ` \u{2022} ${formatTranspose(p.transpose)}` : ""}
                      {p.swing != null ? ` \u{2022} ${i18n.t("tooltip.swing")} ${p.swing}%` : ""}
                    </div>
                  </div>
                  {/* Stats */}
                  <div style={{ "font-size": "var(--font-size-xs)", color: "var(--text-muted)", display: "flex", "flex-direction": "column", "align-items": "flex-end", "flex-shrink": "0" }}>
                    {noteCount > 0 && <span>{noteCount} notes</span>}
                    {hitCount > 0 && <span>{hitCount} hits</span>}
                    {noteCount === 0 && hitCount === 0 && <span>empty</span>}
                  </div>
                </div>
              );
            }}
          </For>
        )}
      </div>

      {/* Pattern property editor */}
      {selectedP() && (() => {
        const p = selectedP()!;
        return (
          <div class="pattern-properties" style={{
            "border-top": "1px solid var(--border)",
            padding: "12px",
            "font-size": "var(--font-size-xs)",
          }}>
            {/* Name (editable) */}
            <div style={{ display: "flex", "align-items": "center", "margin-bottom": "8px" }}>
              <span style={{ width: "64px", color: "var(--text-muted)" }}>{i18n.t("ai.provider.name")}</span>
              <input
                type="text"
                value={p.name}
                onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
                onBlur={(e) => {
                  // A refused rename changes nothing, so the field goes back to
                  // the name the document still holds instead of showing the
                  // rejected attempt.
                  const input = e.currentTarget;
                  const newName = input.value.trim();
                  if (newName === p.name) {
                    input.value = p.name;
                    return;
                  }
                  void store.renamePattern(p.name, newName).then((renamed) => {
                    if (!renamed) input.value = p.name;
                  });
                }}
                style={{
                  flex: "1", background: "var(--bg-input)", color: "var(--text-primary)",
                  border: "1px solid var(--border)", "border-radius": "3px",
                  padding: "3px 8px", "font-size": "var(--font-size-sm)", "font-weight": "600",
                }}
              />
            </div>

            {/* Preset (editable dropdown) */}
            <div class="pattern-preset-row" style={{ display: "flex", "align-items": "center", "margin-bottom": "8px" }}>
              <span
                class="pattern-preset-label"
                style={{ width: "64px", color: "var(--text-muted)" }}
                {...useTooltip(() => i18n.t("pattern.presetHint"))}
              >{i18n.t("pattern.preset")}</span>
              <select
                class="pattern-preset-select"
                aria-label={i18n.t("pattern.preset")}
                value={patternPresetValue(p)}
                disabled={presetPending(p.name)}
                onChange={(e) => {
                  const select = e.currentTarget;
                  void submitPreset(p, select, select.value || null);
                }}
                style={{
                  flex: "1", background: "var(--bg-input)", color: "var(--text-primary)",
                  border: "1px solid var(--border)", "border-radius": "3px",
                  padding: "3px 6px", "font-size": "var(--font-size-xs)",
                }}
              >
                <option value="">{i18n.t("pattern.presetNone")}</option>
                {store.project.presets.map(pr => (
                  <option value={pr.name}>{pr.name} ({pr.source})</option>
                ))}
              </select>
              <Show when={presetPending(p.name)}>
                <span class="pattern-preset-pending" style={{ "margin-left": "6px", color: "var(--text-muted)" }}>
                  {i18n.t("pattern.presetPending")}
                </span>
              </Show>
            </div>
            <Show when={presetFailure()?.pattern === p.name}>
              <div class="pattern-preset-error" role="alert" style={{ color: "var(--danger)", "margin-bottom": "8px" }}>
                {presetFailure()?.message}
              </div>
            </Show>

            {/* Length in bars. Shortening asks what happens to the notes the
                new end no longer covers; keeping them is one undo entry, and
                trimming them is another. */}
            <div style={{ display: "flex", "align-items": "center", "margin-bottom": "6px" }}>
              <span style={{ width: "64px", color: "var(--text-muted)" }}>{i18n.t("pattern.length")}</span>
              <input
                class="pattern-length-input"
                type="number"
                min="0.25"
                step="1"
                value={patternLengthBars(p)}
                onChange={(e) => {
                  const input = e.currentTarget;
                  const current = patternLengthBars(p);
                  const next = parseFloat(input.value);
                  if (!Number.isFinite(next) || next <= 0 || next === current) {
                    input.value = String(current);
                    return;
                  }
                  if (next < current) {
                    // The field goes back to the authored length until the
                    // choice is made, so it never advertises an unapplied value.
                    input.value = String(current);
                    setLengthFailure(null);
                    setLengthPrompt({ pattern: p.name, bars: next });
                    return;
                  }
                  void store.setPatternBars(p.name, next);
                }}
                style={{
                  width: "56px", background: "var(--bg-input)", color: "var(--text-primary)",
                  border: "1px solid var(--border)", "border-radius": "3px",
                  padding: "2px 6px", "font-size": "var(--font-size-xs)",
                }}
              />
              <span style={{ "margin-left": "4px", color: "var(--text-muted)" }}>{i18n.t("pattern.lengthUnit")}</span>
            </div>
            <Show when={lengthPrompt()?.pattern === p.name}>
              {(() => {
                const prompt = lengthPrompt()!;
                const resolve = (action: "keep" | "trim" | "cancel") => {
                  setLengthPrompt(null);
                  if (action === "cancel") return;
                  const write = action === "keep"
                    ? store.setPatternBars(prompt.pattern, prompt.bars)
                    : store.trimPatternToBars(prompt.pattern, prompt.bars);
                  void write.then((failure) => { if (failure) setLengthFailure(failure); });
                };
                return (
                  <div
                    class="pattern-length-prompt"
                    role="dialog"
                    aria-label={i18n.t("pattern.shortenTitle")}
                    style={{
                      border: "1px solid var(--border)", "border-radius": "4px",
                      padding: "6px", "margin-bottom": "6px", background: "var(--bg-input)",
                    }}
                  >
                    <div style={{ color: "var(--text-primary)", "margin-bottom": "4px" }}>
                      {i18n.t("pattern.shortenBody")}
                    </div>
                    <div style={{ display: "flex", gap: "4px", "flex-wrap": "wrap" }}>
                      <button class="toolbar-btn" style={{ "font-size": "11px" }} onClick={() => resolve("keep")}>
                        {i18n.t("pattern.keepNotes")}
                      </button>
                      <button class="toolbar-btn" style={{ "font-size": "11px" }} onClick={() => resolve("trim")}>
                        {i18n.t("pattern.trimNotes")}
                      </button>
                      <button class="toolbar-btn" style={{ "font-size": "11px" }} onClick={() => resolve("cancel")}>
                        {i18n.t("dialog.cancel")}
                      </button>
                    </div>
                  </div>
                );
              })()}
            </Show>
            <Show when={lengthFailure()}>
              <div class="pattern-length-error" role="alert" style={{ color: "var(--danger)", "margin-bottom": "6px" }}>
                {lengthFailure()}
              </div>
            </Show>

            {/* Transpose */}
            <div style={{ display: "flex", "align-items": "center", "margin-bottom": "6px" }}>
              <span style={{ width: "64px", color: "var(--text-muted)" }}>{i18n.t("tooltip.transpose")}</span>
              <input
                type="number" min="-24" max="24"
                value={p.transpose ?? 0}
                onInput={(e) => {
                  const v = parseInt(e.currentTarget.value) || 0;
                  store.setPatternTranspose(p.name, v);
                }}
                style={{
                  width: "56px", background: "var(--bg-input)", color: "var(--text-primary)",
                  border: "1px solid var(--border)", "border-radius": "3px",
                  padding: "2px 6px", "font-size": "var(--font-size-xs)",
                }}
              />
              <span style={{ "margin-left": "4px", color: "var(--text-muted)" }}>{i18n.t("pattern.semitones")}</span>
            </div>

            {/* Swing */}
            <div style={{ display: "flex", "align-items": "center", "margin-bottom": "6px" }}>
              <span style={{ width: "64px", color: "var(--text-muted)" }}>{i18n.t("tooltip.swing")}</span>
              <input
                type="number" min="0" max="100" step="1"
                aria-label={i18n.t("tooltip.swing")}
                aria-describedby={swingHelpId}
                value={p.swing ?? ""}
                onChange={async (e) => {
                  const input = e.currentTarget;
                  const submitted = input.value;
                  const number = input.valueAsNumber;
                  const previous = p.swing ?? null;
                  if (input.validity.badInput || (submitted !== "" && !Number.isFinite(number))) {
                    input.value = previous === null ? "" : String(previous);
                    return;
                  }
                  const swing = submitted === "" ? null : Math.max(0, Math.min(100, number));
                  if (swing === previous) {
                    input.value = swing === null ? "" : String(swing);
                    return;
                  }
                  const failure = await store.setPatternSwing(p.name, swing);
                  if (failure && input.value === submitted) {
                    input.value = p.swing == null ? "" : String(p.swing);
                  }
                }}
                style={{
                  width: "48px", background: "var(--bg-input)", color: "var(--text-primary)",
                  border: "1px solid var(--border)", "border-radius": "3px",
                  padding: "2px 6px", "font-size": "var(--font-size-xs)",
                }}
              />
              <span style={{ "margin-left": "4px", color: "var(--text-muted)" }}>%</span>
              <span id={swingHelpId} style={{ "margin-left": "8px", color: "var(--text-muted)", "font-size": "var(--font-size-xs)" }}>
                {p.swing != null
                  ? i18n.t("pattern.swingClear")
                  : store.project.properties.swing != null
                    ? i18n.t("pattern.swingProject", { value: store.project.properties.swing })
                    : i18n.t("pattern.swingStraight", { value: 50 })}
              </span>
            </div>

            {/* Project swing grid */}
            <div style={{ display: "flex", "align-items": "center", "margin-bottom": "6px" }}>
              <span style={{ width: "64px", color: "var(--text-muted)" }}>{i18n.t("tooltip.swingSubdivision")}</span>
              <select
                aria-label={i18n.t("tooltip.swingSubdivision")}
                value={store.project.properties.swing_subdivision === "sixteenth" ? "16th" : "8th"}
                onChange={async (e) => {
                  const select = e.currentTarget;
                  const failure = await store.setProjectSwingSubdivision(select.value as "8th" | "16th");
                  if (failure) {
                    select.value = store.project.properties.swing_subdivision === "sixteenth" ? "16th" : "8th";
                  }
                }}
                style={{
                  background: "var(--bg-input)", color: "var(--text-primary)",
                  border: "1px solid var(--border)", "border-radius": "3px",
                  padding: "2px 6px", "font-size": "var(--font-size-xs)",
                }}
              >
                <option value="8th">{i18n.t("pattern.swingEighth")}</option>
                <option value="16th">{i18n.t("pattern.swingSixteenth")}</option>
              </select>
            </div>

            {/* Color */}
            <div style={{ "margin-bottom": "4px" }}>
              <span style={{ color: "var(--text-muted)" }}>{i18n.t("device.category.color")}</span>
              <div style={{ display: "flex", "flex-wrap": "wrap", gap: "4px", "margin-top": "4px" }}>
                {COLOR_PRESETS.map(color => (
                  <div
                    data-pattern-color={color}
                    onClick={() => store.setPatternColor(p.name, color)}
                    style={{
                      width: "20px", height: "20px", "border-radius": "3px",
                      background: color,
                      cursor: "pointer",
                      border: p.color === color ? "2px solid white" : "1px solid var(--border)",
                    }}
                    {...useTooltip(() => color)}
                  />
                ))}
                {p.color && (
                  <div
                    onClick={() => store.setPatternColor(p.name, "")}
                    style={{
                      width: "20px", height: "20px", "border-radius": "3px",
                      cursor: "pointer",
                      border: "1px dashed var(--border)",
                      display: "flex", "align-items": "center", "justify-content": "center",
                      color: "var(--text-muted)", "font-size": "10px",
                    }}
                    {...useTooltip(() => "Clear color")}
                  >{"×"}</div>
                )}
              </div>
            </div>
          </div>
        );
      })()}
      <Show when={renamePrompt()}>
        {(name) => (
          <TextPromptDialog
            title={i18n.t("pattern.renameTitle")}
            initial={name()}
            onCancel={() => setRenamePrompt(null)}
            onConfirm={(next) => {
              const from = name();
              setRenamePrompt(null);
              void store.renamePattern(from, next);
            }}
          />
        )}
      </Show>
    </div>
  );
}
