import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { AiMarkdown, safeMarkdownHref } from "../components/AiMarkdown";

describe("safe assistant Markdown", () => {
  it("renders headings, emphasis, nested lists, quotes, tables and inline code", () => {
    const { container } = render(() => <AiMarkdown content={'## Plan\n\n**bold** and *soft* with `tempo: 76`\n\n- first\n  - nested\n- second\n\n> quoted\n\n| Track | Gain |\n| --- | --- |\n| Piano | -6 dB |'} />);
    expect(container.querySelector("h2")?.textContent).toBe("Plan");
    expect(container.querySelector("strong")?.textContent).toBe("bold");
    expect(container.querySelector("em")?.textContent).toBe("soft");
    expect(container.querySelector("code")?.textContent).toBe("tempo: 76");
    expect(container.querySelector("li ul li")?.textContent).toBe("nested");
    expect(container.querySelector("blockquote")?.textContent).toContain("quoted");
    expect(container.querySelectorAll("td")).toHaveLength(2);
  });
  it("shows raw HTML as text and never loads Markdown images", () => {
    const input = '<img src=x onerror=alert(1)>\n\n<script>alert(1)</script>\n\n![picture](https://remote.invalid/pixel.png)';
    const { container } = render(() => <AiMarkdown content={input} />);
    expect(container.querySelector("img,script,iframe")).toBeNull();
    expect(container.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(container.textContent).toContain("picture");
  });
  it("allows only external HTTP(S), decodes entities before checking, and isolates new windows", () => {
    for (const href of ["javascript:alert(1)", "jav&#x61;script:alert(1)", "data:text/html,x", "file:///tmp/x", "/relative", "//host/x"]) expect(safeMarkdownHref(href)).toBeUndefined();
    const { container } = render(() => <AiMarkdown content={'[docs](https://example.com/?a=1&amp;b=2) and [bad](javascript:alert%281%29)'} />);
    const links = container.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0].href).toBe("https://example.com/?a=1&b=2");
    expect(links[0].target).toBe("_blank");
    expect(links[0].rel).toBe("noopener noreferrer");
    expect(container.textContent).toContain("bad");
  });
  it("renders escaped punctuation and entities as inert readable text", () => {
    const { container } = render(() => <AiMarkdown content={'\\*literal\\* &amp; &#x3c;img&#x3e; `&amp;`'} />);
    expect(container.textContent).toBe('*literal* & <img> &amp;');
    expect(container.querySelector("img,em")).toBeNull();
  });
  it("reuses safe highlighted JSON/DSL fences and preserves unknown code", () => {
    const { container } = render(() => <AiMarkdown content={'```json\n{"name":"<script>","tempo":76}\n```\n\n```dsl\n@project "Piano"\ntempo: 76\n```\n\n```other\n<b>raw</b>\n```'} />);
    expect([...container.querySelectorAll("pre")].map(e => e.textContent)).toEqual(['{"name":"<script>","tempo":76}', '@project "Piano"\ntempo: 76', '<b>raw</b>']);
    expect(container.querySelector(".syn-property")).not.toBeNull();
    expect(container.querySelector(".syn-keyword")).not.toBeNull();
    expect(container.querySelector("script,b")).toBeNull();
  });
  it("updates streamed content without keeping the earlier partial parse", () => {
    const [content, setContent] = createSignal("**par");
    const { container } = render(() => <AiMarkdown content={content()} />);
    setContent("**paragraph**");
    expect(container.querySelector("strong")?.textContent).toBe("paragraph");
  });
});
