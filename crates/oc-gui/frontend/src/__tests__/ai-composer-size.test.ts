import { afterEach, expect, it, vi } from "vitest";
import { composerSizer } from "../ai/composerSize";

afterEach(() => { vi.restoreAllMocks(); document.body.replaceChildren(); });

it("measures drafts outside layout and never resets the live field height while typing", async () => {
  const field = document.createElement("textarea");
  field.style.cssText = "border:1px solid;box-sizing:border-box;width:240px";
  document.body.append(field);
  let content = 31;
  vi.spyOn(HTMLTextAreaElement.prototype, "scrollHeight", "get").mockImplementation(function (this: HTMLTextAreaElement) {
    expect(this).not.toBe(field);
    return content;
  });
  const size = composerSizer();
  size.resize(field);
  expect(field.style.height).toBe("33px");
  const mutations: MutationRecord[] = [];
  const observer = new MutationObserver(records => mutations.push(...records));
  observer.observe(field, { attributes: true, attributeFilter: ["style"] });
  for (const value of ["a", "ab", "abc", "abcd"]) { field.value = value; size.resize(field); }
  await Promise.resolve();
  expect(mutations).toHaveLength(0);
  expect(document.querySelectorAll(".ai-input-measure")).toHaveLength(1);
  content = 200;
  size.resize(field);
  expect(field.style.height).toBe("160px");
  content = 31;
  field.value = "";
  size.resize(field);
  expect(field.style.height).toBe("33px");
  expect(document.querySelector(".ai-input-measure")!.getAttribute("aria-hidden")).toBe("true");
  size.dispose();
  expect(document.querySelector(".ai-input-measure")).toBeNull();
  observer.disconnect();
});
