import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { ToolIcon } from "../components/ToolIcon";

describe("tool icons", () => {
  it("draws a node in every row, not only the first", () => {
    const view = render(() => (
      <div>
        <span class="a"><ToolIcon name="sample_search" /></span>
        <span class="b"><ToolIcon name="sample_search" /></span>
        <span class="c"><ToolIcon name="devices_catalog" /></span>
      </div>
    ));
    for (const selector of [".a svg", ".b svg", ".c svg"]) {
      expect(view.container.querySelector(selector), selector).toBeTruthy();
      expect(view.container.querySelector(selector)!.children.length).toBeGreaterThan(0);
    }
  });
});
