import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { AiToolContent, CODE_PAGE_SIZE } from "../components/AiToolContent";
import { i18n } from "../stores/i18n";

describe("formatted tool details", () => {
  it("mounts syntax nodes only while the owning tool details are expanded", () => {
    const view = render(() => <details><summary>Tool</summary><AiToolContent content={'{"ok":true}'} /></details>);
    const details = view.container.querySelector("details")!;
    expect(view.container.querySelector("pre")).toBeNull();
    details.open = true;
    fireEvent(details, new Event("toggle"));
    expect(view.container.querySelector("pre")).not.toBeNull();
    details.open = false;
    fireEvent(details, new Event("toggle"));
    expect(view.container.querySelector("pre")).toBeNull();
  });
  it("keeps surrogate pairs intact at page boundaries", () => {
    i18n.setLocale("en");
    const content = "a".repeat(CODE_PAGE_SIZE - 1) + "🎹end";
    const view = render(() => <AiToolContent content={content} />);
    const first = view.container.querySelector("pre")!.textContent!;
    fireEvent.click(view.getByText("Next"));
    const second = view.container.querySelector("pre")!.textContent!;
    expect(second.startsWith("🎹")).toBe(true);
    expect(first + second).toBe(content);
  });
  it("pretty prints JSON and renders decoded DSL with safe syntax text", () => {
    const dsl = '@project "<img src=x onerror=alert(1)>" {\n tempo: 76\n}';
    const view = render(() => <AiToolContent content={JSON.stringify({dsl_source:dsl, dirty:true})} />);
    const blocks = view.container.querySelectorAll("pre");
    expect(blocks[0].textContent).toBe(JSON.stringify({dsl_source:dsl,dirty:true},null,2));
    expect(blocks[1].textContent).toBe(dsl);
    expect(view.container.querySelector("img")).toBeNull();
    expect(view.container.querySelector(".syn-property")).not.toBeNull();
    expect(blocks[1].querySelector(".syn-keyword")?.textContent).toBe("@project");
  });
  it("paginates long results without losing text or creating an unbounded DOM", () => {
    i18n.setLocale("en");
    const content = "a".repeat(CODE_PAGE_SIZE) + "last page";
    const view = render(() => <AiToolContent content={content} />);
    expect(view.container.querySelector("pre")?.textContent?.length).toBe(CODE_PAGE_SIZE);
    expect(view.getByText("Previous")).toBeDisabled();
    fireEvent.click(view.getByText("Next"));
    expect(view.container.querySelector("pre")?.textContent).toBe("last page");
    expect(view.getByText("Next")).toBeDisabled();
    fireEvent.click(view.getByText("Previous"));
    expect(view.container.querySelector("pre")?.textContent?.length).toBe(CODE_PAGE_SIZE);
  });
  it("keeps malformed JSON readable and recognizes direct DSL", () => {
    const view = render(() => <AiToolContent content={'@track "piano" { C4 q }'} />);
    expect(view.getByText("DSL")).toBeTruthy();
    expect(view.container.querySelector(".syn-pitch")?.textContent).toBe("C4");
  });
});
