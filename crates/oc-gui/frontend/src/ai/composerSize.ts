/** Measure outside normal flow so typing never shrinks the transcript and then
 * expands it again just to discover the textarea's intrinsic height. */
export function composerSizer() {
  let measure: HTMLTextAreaElement | undefined;
  return {
    resize(element: HTMLTextAreaElement | undefined) {
      if (!element?.isConnected) return;
      if (!measure) {
        measure = element.cloneNode(false) as HTMLTextAreaElement;
        measure.removeAttribute("id");
        measure.removeAttribute("placeholder");
        measure.className = "ai-input-measure";
        measure.setAttribute("aria-hidden", "true");
        measure.tabIndex = -1;
      }
      const style = getComputedStyle(element);
      // Keep the same inherited font and box metrics at every UI scale.
      measure.style.cssText = style.cssText;
      for (const property of ["font", "font-size", "font-family", "font-weight", "line-height", "letter-spacing", "padding", "border-width", "box-sizing", "width", "word-break", "overflow-wrap"]) {
        measure.style.setProperty(property, style.getPropertyValue(property));
      }
      Object.assign(measure.style, { position: "absolute", visibility: "hidden", pointerEvents: "none", height: "0px", minHeight: "0px", maxHeight: "none", overflow: "hidden", resize: "none" });
      element.parentElement?.append(measure);
      measure.value = element.value;
      const border = (Number.parseFloat(style.borderTopWidth) || 0) + (Number.parseFloat(style.borderBottomWidth) || 0);
      const height = Math.min(Math.ceil(measure.scrollHeight + border), 160);
      if (height > border && element.style.height !== `${height}px`) element.style.height = `${height}px`;
    },
    dispose() { measure?.remove(); measure = undefined; },
  };
}
