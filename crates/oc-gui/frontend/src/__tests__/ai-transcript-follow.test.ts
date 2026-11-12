import { afterEach, describe, expect, it, vi } from "vitest";
import { followTranscript } from "../ai/transcriptScroll";

afterEach(() => { document.body.replaceChildren(); vi.unstubAllGlobals(); });
function fixture() {
  const element = document.createElement("div");
  document.body.append(element);
  let height = 1000;
  Object.defineProperty(element, "scrollHeight", { get: () => height });
  Object.defineProperty(element, "clientHeight", { value: 200 });
  element.scrollTop = 800;
  const follow = followTranscript(element);
  return { element, follow, grow() { height += 500; element.append(document.createElement("p")); } };
}
const settle = async () => { await Promise.resolve(); await Promise.resolve(); };
describe("transcript bottom following", () => {
  it("follows appended turns after a long diff but preserves manual upward scroll", async () => {
    const f = fixture();
    f.grow(); await settle();
    expect(f.element.scrollTop).toBe(1500);
    f.element.scrollTop = 120;
    f.element.dispatchEvent(new Event("scroll"));
    f.grow(); await settle();
    expect(f.element.scrollTop).toBe(120);
    f.follow.resume(); await settle(); // Explicit Send / Accept / Reject / restore.
    expect(f.element.scrollTop).toBe(2000);
    f.follow.dispose();
  });
  it("resumes when the reader scrolls back to bottom", async () => {
    const f = fixture();
    f.element.scrollTop = 0; f.element.dispatchEvent(new Event("scroll"));
    f.element.scrollTop = 790; f.element.dispatchEvent(new Event("scroll"));
    f.grow(); await settle();
    expect(f.element.scrollTop).toBe(1500);
    f.follow.dispose();
  });
  it("follows footer-driven resize only while pinned and disconnects on disposal", async () => {
    let resize = () => {};
    const disconnect = vi.fn();
    vi.stubGlobal("ResizeObserver", class { constructor(callback: () => void) { resize = callback; } observe() {} disconnect = disconnect; });
    const f = fixture();
    resize(); await settle(); expect(f.element.scrollTop).toBe(1000);
    f.element.scrollTop = 120; f.element.dispatchEvent(new Event("scroll"));
    resize(); await settle(); expect(f.element.scrollTop).toBe(120);
    f.follow.dispose(); expect(disconnect).toHaveBeenCalledOnce();
    f.follow.resume(); f.grow(); await settle(); expect(f.element.scrollTop).toBe(120);
  });
});
