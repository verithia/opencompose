import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { Brand } from "../../components/Brand";

describe("Brand", () => {
  it("spans the toolbar and the ruler so the mark has room", () => {
    const { container } = render(() => <Brand />);
    const block = container.querySelector(".brand-block")!;
    expect(block).toBeTruthy();
    expect(block.hasAttribute("data-tauri-drag-region")).toBe(true);
    expect(block.getAttribute("data-tauri-drag-region")).toBe("deep");
    expect(block.querySelector(".brand-lights")).toBeTruthy();
    expect(block.querySelector(".app-logo")).toBeTruthy();
    expect(block.querySelector(".brand-name")!.textContent).toBe("OpenCompose");
  });
});
