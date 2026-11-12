import { createMemo, For, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { marked, type Token, type Tokens } from "marked";
import { CodeBlock } from "./AiToolContent";

/** Only isolated character references enter this decoder, never markup. */
function decodeEntities(text: string): string {
  return text.replace(/&(?:#[0-9]+|#x[0-9a-f]+|[a-z][a-z0-9]+);/gi, entity => {
    const decoder = document.createElement("textarea");
    decoder.innerHTML = entity;
    return decoder.value;
  });
}

/** Model content never becomes HTML; unsupported/raw tokens remain visible text. */
export function safeMarkdownHref(href: string): string | undefined {
  try {
    const url = new URL(decodeEntities(href));
    return ["https:", "http:"].includes(url.protocol) ? url.href : undefined;
  } catch { return undefined; }
}
function inline(tokens: Token[] | undefined): JSX.Element {
  return <For each={tokens}>{token => renderToken(token)}</For>;
}
function renderToken(token: Token): JSX.Element {
  switch (token.type) {
    case "space": return null;
    case "heading": { const t = token as Tokens.Heading; return <Dynamic component={`h${t.depth}`}>{inline(t.tokens)}</Dynamic>; }
    case "paragraph": return <p>{inline((token as Tokens.Paragraph).tokens)}</p>;
    case "text": { const t = token as Tokens.Text; return t.tokens ? inline(t.tokens) : decodeEntities(t.text); }
    case "escape": return (token as Tokens.Escape).text;
    case "strong": return <strong>{inline((token as Tokens.Strong).tokens)}</strong>;
    case "em": return <em>{inline((token as Tokens.Em).tokens)}</em>;
    case "del": return <del>{inline((token as Tokens.Del).tokens)}</del>;
    case "codespan": return <code>{(token as Tokens.Codespan).text}</code>;
    case "br": return <br />;
    case "hr": return <hr />;
    case "blockquote": return <blockquote>{inline((token as Tokens.Blockquote).tokens)}</blockquote>;
    case "list": {
      const t = token as Tokens.List;
      return <Dynamic component={t.ordered ? "ol" : "ul"} start={t.ordered ? t.start : undefined}>
        <For each={t.items}>{item => <li>{item.task && <input type="checkbox" checked={!!item.checked} disabled />}{inline(item.tokens)}</li>}</For>
      </Dynamic>;
    }
    case "link": {
      const t = token as Tokens.Link, href = safeMarkdownHref(t.href);
      return href ? <a href={href} target="_blank" rel="noopener noreferrer" title={t.title ?? undefined}>{inline(t.tokens)}</a> : inline(t.tokens);
    }
    case "image": return decodeEntities((token as Tokens.Image).text); // No remote requests or tracking pixels.
    case "code": {
      const t = token as Tokens.Code;
      const language = (t.lang ?? "").split(/\s/)[0].toLowerCase();
      return <CodeBlock language={language === "json" ? "JSON" : ["dsl", "ocp", "opencompose"].includes(language) ? "DSL" : "Text"} text={t.text} />;
    }
    case "table": {
      const t = token as Tokens.Table;
      return <div class="ai-markdown-table"><table><thead><tr><For each={t.header}>{cell => <th>{inline(cell.tokens)}</th>}</For></tr></thead>
        <tbody><For each={t.rows}>{row => <tr><For each={row}>{cell => <td>{inline(cell.tokens)}</td>}</For></tr>}</For></tbody></table></div>;
    }
    default: return token.raw;
  }
}
export function AiMarkdown(props: { content: string }) {
  const tokens = createMemo(() => marked.lexer(props.content, { gfm: true }));
  return <div class="ai-markdown">{inline(tokens())}</div>;
}
