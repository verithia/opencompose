/**
 * Escape belongs to the topmost dialog. Close callbacks sit on a stack, so a
 * modal opened from another modal takes the key and the one below it stays.
 */
import { createSignal, onCleanup, onMount } from "solid-js";

const stack: (() => void)[] = [];
const [openDialogs, setOpenDialogs] = createSignal(0);

/** True while a dialog owns Escape, so hover cards can stack above its overlay. */
export const dialogOpen = () => openDialogs() > 0;

export function useTopmostEscape(close: () => void, canClose: () => boolean = () => true) {
  const entry = () => {
    if (canClose()) close();
  };
  onMount(() => {
    stack.push(entry);
    setOpenDialogs((count) => count + 1);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (stack[stack.length - 1] !== entry) return;
      event.preventDefault();
      event.stopPropagation();
      entry();
    };
    document.addEventListener("keydown", onKeyDown, true);
    onCleanup(() => {
      document.removeEventListener("keydown", onKeyDown, true);
      const index = stack.indexOf(entry);
      if (index >= 0) stack.splice(index, 1);
      setOpenDialogs((count) => Math.max(0, count - 1));
    });
  });
  return () => stack[stack.length - 1] === entry;
}
