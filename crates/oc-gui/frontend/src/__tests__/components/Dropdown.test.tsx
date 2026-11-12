import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { Show, createSignal } from "solid-js";
import { Dropdown, placeDropdown, type DropdownOption } from "../../components/Dropdown";

const OPTIONS: DropdownOption<number>[] = [
  { value: 1, label: "One" },
  { value: 2, label: "Two" },
  { value: 3, label: "Three" },
  { value: 4, label: "Four" },
];

function Host(props: { onSelect?: (value: number) => void }) {
  const [value, setValue] = createSignal(3);
  return (
    <Dropdown
      label="Voice count"
      dataParam="test.voices"
      value={value()}
      options={OPTIONS}
      onSelect={(next) => {
        setValue(next);
        props.onSelect?.(next);
      }}
    />
  );
}

function trigger(): HTMLButtonElement {
  return document.querySelector(".oc-dropdown-trigger") as HTMLButtonElement;
}

function listbox(): HTMLElement | null {
  return document.querySelector<HTMLElement>('[role="listbox"]');
}

function options(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>('[role="listbox"] [role="option"]')];
}

async function openWithFocus() {
  fireEvent.click(trigger());
  const list = await waitFor(() => {
    const node = listbox();
    expect(node).toBeTruthy();
    return node!;
  });
  await waitFor(() => expect(document.activeElement).toBe(list));
  return list;
}

beforeEach(() => {
  // jsdom has no layout engine, so every element reports an empty rect.
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(() => {
  vi.restoreAllMocks();
  document.querySelector(".test-scroll-panel")?.remove();
});

describe("Dropdown", () => {
  it("opens a labelled listbox that marks the current value", async () => {
    render(() => <Host />);
    const button = trigger();
    expect(button.getAttribute("aria-haspopup")).toBe("listbox");
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button).toHaveAccessibleName("Voice count");
    expect(button.textContent).toContain("Three");
    expect(listbox()).toBeNull();

    const list = await openWithFocus();
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(button.getAttribute("aria-controls")).toBe(list.id);
    expect(list.getAttribute("aria-labelledby")).toBeTruthy();
    expect(list.getAttribute("aria-activedescendant")).toBe(options()[2].id);
    expect(options().map((option) => option.textContent?.trim())).toEqual([
      "One", "Two", "\u2713Three", "Four",
    ]);
    expect(options().map((option) => option.getAttribute("aria-selected"))).toEqual([
      "false", "false", "true", "false",
    ]);
  });

  it("moves the highlight with arrows, scrolls it into view and commits with Enter", async () => {
    const onSelect = vi.fn();
    render(() => <Host onSelect={onSelect} />);
    const list = await openWithFocus();
    expect(list.getAttribute("aria-activedescendant")).toBe(options()[2].id);

    fireEvent.keyDown(list, { key: "ArrowDown" });
    await waitFor(() => expect(list.getAttribute("aria-activedescendant")).toBe(options()[3].id));
    const highlighted = list.querySelector<HTMLElement>('[data-option-index="3"]')!;
    await waitFor(() => expect(highlighted.scrollIntoView).toHaveBeenCalled());
    expect(Element.prototype.scrollIntoView).toHaveBeenLastCalledWith({ block: "nearest" });

    fireEvent.keyDown(list, { key: "Enter" });
    expect(onSelect).toHaveBeenCalledWith(4);
    expect(listbox()).toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    expect(trigger().textContent).toContain("Four");
    expect(document.activeElement).toBe(trigger());
  });

  it("wraps with ArrowUp and supports Home and End", async () => {
    render(() => <Host />);
    const list = await openWithFocus();

    fireEvent.keyDown(list, { key: "ArrowDown" });
    fireEvent.keyDown(list, { key: "ArrowDown" });
    await waitFor(() => expect(list.getAttribute("aria-activedescendant")).toBe(options()[0].id));

    fireEvent.keyDown(list, { key: "End" });
    await waitFor(() => expect(list.getAttribute("aria-activedescendant")).toBe(options()[3].id));

    fireEvent.keyDown(list, { key: "ArrowDown" });
    await waitFor(() => expect(list.getAttribute("aria-activedescendant")).toBe(options()[0].id));

    fireEvent.keyDown(list, { key: "Home" });
    await waitFor(() => expect(list.getAttribute("aria-activedescendant")).toBe(options()[0].id));
  });

  it("commits with Space as well as Enter", async () => {
    const onSelect = vi.fn();
    render(() => <Host onSelect={onSelect} />);
    const list = await openWithFocus();

    fireEvent.keyDown(list, { key: " " });
    expect(onSelect).toHaveBeenCalledWith(3);
    expect(document.activeElement).toBe(trigger());
  });

  it("closes on Escape and restores focus to the trigger without committing", async () => {
    const onSelect = vi.fn();
    render(() => <Host onSelect={onSelect} />);
    await openWithFocus();

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(listbox()).toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    expect(onSelect).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(trigger());
  });

  it("closes on an outside pointer down but not on one inside the list", async () => {
    render(() => <Host />);
    await openWithFocus();

    fireEvent.pointerDown(options()[0]);
    expect(listbox()).toBeTruthy();

    fireEvent.pointerDown(document.body);
    expect(listbox()).toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
  });

  it("renders the popover in a portal outside a scrolling ancestor", async () => {
    const scrollPanel = document.createElement("div");
    scrollPanel.className = "test-scroll-panel";
    scrollPanel.style.overflow = "auto";
    document.body.appendChild(scrollPanel);

    render(() => <Host />, { container: scrollPanel });
    const list = await openWithFocus();

    expect(scrollPanel.contains(list)).toBe(false);
    expect(document.body.contains(list)).toBe(true);
    expect(list.parentElement?.parentElement).toBe(document.body);
  });

  it("flips above the trigger when the list would overflow the viewport", async () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const rect = this.classList.contains("oc-dropdown-menu")
        ? { left: 40, top: 0, width: 120, height: 200 }
        : { left: 40, top: 740, width: 120, height: 24 };
      return { ...rect, right: rect.left + rect.width, bottom: rect.top + rect.height, x: rect.left, y: rect.top, toJSON: () => ({}) } as DOMRect;
    });

    render(() => <Host />);
    fireEvent.click(trigger());
    const list = await waitFor(() => {
      const node = listbox();
      expect(node?.dataset.placement).toBe("above");
      return node!;
    });
    expect(list.style.top).toBe("538px");
    expect(list.style.left).toBe("40px");
    expect(list.style.width).toBe("120px");
    expect(list.style.visibility).toBe("visible");
  });

  it("keeps the opening Space press from reaching global shortcuts", async () => {
    const globalKey = vi.fn();
    document.addEventListener("keydown", globalKey);
    render(() => <Host />);

    fireEvent.keyDown(trigger(), { key: " " });
    await waitFor(() => expect(listbox()).toBeTruthy());
    expect(globalKey).not.toHaveBeenCalled();

    document.removeEventListener("keydown", globalKey);
  });

  it("is reachable from the keyboard on the trigger and toggles closed", async () => {
    render(() => <Host />);
    const button = trigger();

    fireEvent.keyDown(button, { key: "ArrowDown" });
    await waitFor(() => expect(listbox()).toBeTruthy());

    fireEvent.click(button);
    expect(listbox()).toBeNull();
    expect(button.getAttribute("aria-expanded")).toBe("false");
  });
});

describe("placeDropdown", () => {
  const viewport = { width: 800, height: 768 };
  const anchor = { left: 100, top: 200, width: 140, height: 24 };

  it("opens below when there is room and matches the trigger width", () => {
    expect(placeDropdown(anchor, { width: 100, height: 240 }, viewport)).toEqual({
      x: 100, y: 226, width: 140, placement: "below",
    });
  });

  it("flips above when the list would overflow the bottom edge", () => {
    const placed = placeDropdown({ left: 100, top: 700, width: 140, height: 24 }, { width: 140, height: 240 }, viewport);
    expect(placed.placement).toBe("above");
    expect(placed.y).toBe(458);
  });

  it("clamps to the right edge and never exceeds the viewport width", () => {
    const placed = placeDropdown({ left: 760, top: 200, width: 300, height: 24 }, { width: 300, height: 120 }, viewport);
    expect(placed.width).toBe(300);
    expect(placed.x).toBe(494);
  });
});


describe("Dropdown lifetime admission", () => {
  it("keeps retained option and trigger nodes inert after unmount", async () => {
    const onOpen = vi.fn();
    const onClose = vi.fn();
    const onSelect = vi.fn();
    const view = render(() => <Dropdown label="Owned choice" value={1} options={OPTIONS}
      onOpen={onOpen} onClose={onClose} onSelect={onSelect} />);
    await openWithFocus();
    const oldTrigger = trigger();
    const oldOption = options()[1];
    view.unmount();
    expect(onClose).toHaveBeenCalledTimes(1);
    // Reattach retained nodes so Solid's delegated events reach their handlers.
    document.body.append(oldTrigger, oldOption);
    try {
      fireEvent.click(oldOption);
      fireEvent.click(oldTrigger);
      fireEvent.keyDown(oldTrigger, { key: "ArrowDown" });
      expect(onSelect).not.toHaveBeenCalled();
      expect(onOpen).toHaveBeenCalledTimes(1);
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(listbox()).toBeNull();
    } finally { oldTrigger.remove(); oldOption.remove(); }
  });

  it("does not publish a menu when its opening hook unmounts the host", () => {
    const [visible, setVisible] = createSignal(true);
    const onClose = vi.fn();
    const onSelect = vi.fn();
    render(() => <Show when={visible()}><Dropdown label="Owned choice" value={1} options={OPTIONS}
      onOpen={() => setVisible(false)} onClose={onClose} onSelect={onSelect} /></Show>);
    fireEvent.click(trigger());
    expect(listbox()).toBeNull();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("does not select after focus restoration synchronously unmounts its host", async () => {
    const [visible, setVisible] = createSignal(true);
    const onClose = vi.fn();
    const onSelect = vi.fn();
    render(() => <Show when={visible()}><Dropdown label="Owned choice" value={1} options={OPTIONS}
      onClose={onClose} onSelect={onSelect} /></Show>);
    await openWithFocus();
    trigger().addEventListener("focus", () => setVisible(false), { once: true });
    fireEvent.click(options()[1]);
    expect(onSelect).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(listbox()).toBeNull();
  });
});
