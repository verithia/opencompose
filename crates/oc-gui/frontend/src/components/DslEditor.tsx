import { createSignal, createMemo, createEffect, on, onCleanup, Show } from "solid-js";
import { store } from "../stores/project";
import { highlightOcp } from "../editor/highlight";
import { useTooltip } from "./Tooltip";
import { i18n } from "../stores/i18n";

/**
 * The unsaved draft survives a panel switch: the editor unmounts whenever the
 * sidebar shows another tab, and losing typed DSL without a word is not a
 * cancel the user asked for.
 */
let rememberedDraft: { base: string; source: string } | null = null;

/**
 * DSL code editor with syntax highlighting.
 *
 * Uses an overlay approach: a transparent textarea on top of a pre element
 * with highlighted HTML. The textarea handles input, the pre shows colors.
 */
export function DslEditor() {
  let textareaRef: HTMLTextAreaElement | undefined;
  let preRef: HTMLPreElement | undefined;
  const restoredDraft = rememberedDraft && rememberedDraft.base === store.dslSource()
    ? rememberedDraft.source
    : store.dslSource();
  const [localSource, setLocalSource] = createSignal(restoredDraft);
  const [parseError, setParseError] = createSignal<string | null>(null);
  const [applying, setApplying] = createSignal(false);

  // Sync from store -> local only when the project itself changes; the effect
  // also runs on mount, which must not discard a remembered draft.
  let lastStoreSource = store.dslSource();
  createEffect(on(store.dslSource, (source) => {
    if (source === lastStoreSource) return;
    lastStoreSource = source;
    rememberedDraft = null;
    setLocalSource(source);
  }));

  onCleanup(() => {
    rememberedDraft = localSource() === store.dslSource()
      ? null
      : { base: store.dslSource(), source: localSource() };
  });

  const dirty = () => localSource() !== store.dslSource();

  const apply = async () => {
    if (!dirty() || applying()) return;
    setApplying(true);
    try {
      await store.updateDsl(localSource());
      setParseError(null);
    } catch (err) {
      setParseError(String(err));
    } finally {
      setApplying(false);
    }
  };

  // Highlighted HTML
  const highlighted = createMemo(() => highlightOcp(localSource()) + "\n");

  // Sync scroll between textarea and pre
  const syncScroll = () => {
    if (textareaRef && preRef) {
      preRef.scrollTop = textareaRef.scrollTop;
      preRef.scrollLeft = textareaRef.scrollLeft;
    }
  };

  const handleInput = (e: InputEvent) => {
    const source = (e.target as HTMLTextAreaElement).value;
    setLocalSource(source);
    rememberedDraft = { base: store.dslSource(), source };
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Tab") {
      e.preventDefault();
      const textarea = textareaRef!;
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const value = textarea.value;
      const newValue = value.substring(0, start) + "  " + value.substring(end);
      setLocalSource(newValue);
      rememberedDraft = { base: store.dslSource(), source: newValue };
      requestAnimationFrame(() => {
        textarea.selectionStart = textarea.selectionEnd = start + 2;
      });
    }
    // Cmd+Enter / Ctrl+Enter to parse and apply
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void apply();
    }
  };

  return (
    <div class="dsl-editor-panel">
      <div class="dsl-editor-toolbar">
        <span style={{ "font-size": "var(--font-size-sm)", color: "var(--text-secondary)" }}>
          {i18n.t("dsl.source")}
        </span>
        <Show when={dirty()}>
          <span class="dsl-editor-modified">{i18n.t("dsl.modified")}</span>
        </Show>
        <span style={{ flex: "1" }} />
        {parseError() && (
          <span
            class="selectable dsl-editor-error"
            role="alert"
            style={{
              "font-size": "var(--font-size-xs)",
              color: "var(--stop-red)",
              "max-width": "400px",
              overflow: "hidden",
              "text-overflow": "ellipsis",
              "white-space": "nowrap",
            }}
            {...useTooltip(() => parseError()!)}
          >
            {parseError()}
          </span>
        )}
        <button
          type="button"
          class="toolbar-btn text-btn dsl-apply"
          aria-label={i18n.t("dsl.apply")}
          classList={{ active: dirty() }}
          disabled={!dirty() || applying()}
          onClick={() => void apply()}
          title={dirty() ? undefined : i18n.t("dsl.noChanges")}
          {...useTooltip(() => dirty() ? i18n.t("dsl.applyHint") : i18n.t("dsl.noChanges"))}
        >
          {applying() ? i18n.t("dsl.applying") : i18n.t("dsl.apply")}
        </button>
      </div>

      <div class="dsl-editor-container">
        {/* Highlighted layer (behind) */}
        <pre
          ref={preRef}
          class="dsl-highlight-layer"
          innerHTML={highlighted()}
        />

        {/* Input layer (on top, transparent text) */}
        <textarea
          ref={textareaRef}
          class="dsl-textarea dsl-input-layer"
          value={localSource()}
          onInput={handleInput}
          onKeyDown={handleKeyDown}
          onScroll={syncScroll}
          spellcheck={false}
          aria-label={i18n.t("dsl.source")}
          placeholder={`// Type or paste OpenCompose DSL here...
@project "My Song"
  tempo: 120
  time_signature: 4/4
  key: C major

@track piano | instrument: piano/acoustic_grand
  @phrase intro [1:1:0 - 4:4:0]
    C4  q   mf
    E4  q   mf  articulation=legato
    G4  h   f`}
        />
      </div>
    </div>
  );
}
