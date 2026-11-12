import { For, Show, createMemo, createSignal, onMount, onCleanup } from "solid-js";
import { i18n } from "../stores/i18n";

export const CODE_PAGE_SIZE = 12_000;
type Language = "JSON" | "DSL" | "Text";
interface Block { language: Language; text: string }

export function toolCodeBlocks(content: string): Block[] {
  try {
    const parsed: unknown = JSON.parse(content);
    const blocks: Block[] = [{ language: "JSON", text: JSON.stringify(parsed, null, 2) }];
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      for (const key of ["dsl_source", "new_dsl"]) {
        const dsl = (parsed as Record<string, unknown>)[key];
        if (typeof dsl === "string" && dsl.trim()) blocks.push({ language: "DSL", text: dsl });
      }
    }
    return blocks;
  } catch {
    return [{ language: /^\s*@(?:project|track|phrase|pattern|preset|section|master|bus)\b/m.test(content) ? "DSL" : "Text", text: content }];
  }
}

/** Lexical color only. Every captured token is rendered as a text node. */
function tokens(text: string, language: Language) {
  if (language === "Text") return [{ text, className: "" }];
  const pattern = language === "JSON"
    ? /"(?:\\.|[^"\\])*"\s*(?=:)|"(?:\\.|[^"\\])*"|\b(?:true|false|null)\b|-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/g
    : /\/\/[^\n]*|"(?:\\.|[^"\\])*"|@[\w]+|\b[\w]+(?=\s*:)|\b[A-G](?:#|b)?\d\b|-?\b\d+(?:\.\d+)?\b/g;
  const out: { text: string; className: string }[] = [];
  let end = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > end) out.push({ text: text.slice(end, match.index), className: "" });
    const token = match[0];
    const after = text.slice(match.index + token.length);
    const className = token.startsWith("//") ? "syn-comment"
      : token.startsWith('"') ? (/^\s*:/.test(after) ? "syn-property" : "syn-string")
      : token.startsWith("@") || /^(true|false|null)$/.test(token) ? "syn-keyword"
      : /^\s*:/.test(after) ? "syn-property"
      : /^[A-G]/.test(token) ? "syn-pitch" : "syn-number-unit";
    out.push({ text: token, className });
    end = match.index + token.length;
  }
  if (end < text.length) out.push({ text: text.slice(end), className: "" });
  return out;
}

export function CodeBlock(props: Block) {
  const [page, setPage] = createSignal(0);
  const pages = createMemo(() => {
    const slices: string[] = [];
    for (let start = 0; start < props.text.length;) {
      let end = Math.min(start + CODE_PAGE_SIZE, props.text.length);
      if (end < props.text.length) {
        const newline = props.text.lastIndexOf("\n", end - 1);
        if (newline > start + CODE_PAGE_SIZE / 2) end = newline + 1;
        const previous = props.text.charCodeAt(end - 1);
        if (previous >= 0xd800 && previous <= 0xdbff) end -= 1;
      }
      slices.push(props.text.slice(start, end));
      start = end;
    }
    return slices.length ? slices : [""];
  });
  const count = () => pages().length;
  const current = () => Math.min(page(), count() - 1);
  const text = () => pages()[current()];
  const highlighted = createMemo(() => tokens(text(), props.language));
  return <section class="ai-tool-code">
    <div class="ai-code-language">{props.language === "Text" ? i18n.t("ai.code.text") : props.language}</div>
    <pre><code><For each={highlighted()}>{token => <span class={token.className || undefined}>{token.text}</span>}</For></code></pre>
    <Show when={count() > 1}><div class="ai-code-pages">
      <button type="button" disabled={current() === 0} onClick={() => setPage(current() - 1)}>{i18n.t("ai.code.previous")}</button>
      <span>{i18n.t("ai.code.page", { page: String(current() + 1), total: String(count()) })}</span>
      <button type="button" disabled={current() + 1 >= count()} onClick={() => setPage(current() + 1)}>{i18n.t("ai.code.next")}</button>
    </div></Show>
  </section>;
}

export function AiToolContent(props: { content: string }) {
  let container!: HTMLDivElement;
  const [visible, setVisible] = createSignal(false);
  onMount(() => {
    const details = container.closest("details");
    if (!details) { setVisible(true); return; }
    const update = () => setVisible(details.open);
    update();
    details.addEventListener("toggle", update);
    onCleanup(() => details.removeEventListener("toggle", update));
  });
  const blocks = createMemo(() => visible() ? toolCodeBlocks(props.content) : []);
  return <div ref={container}><For each={blocks()}>{block => <CodeBlock {...block} />}</For></div>;
}
