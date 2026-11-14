/** Follow new content only while the reader remains near the transcript bottom. */
export function followTranscript(element: HTMLElement) {
  let following = true;
  let queued = false;
  let disposed = false;
  let clientHeight = element.clientHeight;
  let scrollHeight = element.scrollHeight;
  let writtenTop: number | undefined;
  const schedule = () => {
    if (queued || disposed) return;
    queued = true;
    queueMicrotask(() => {
      queued = false;
      if (!disposed && following && element.isConnected) {
        clientHeight = element.clientHeight;
        scrollHeight = element.scrollHeight;
        element.scrollTop = scrollHeight;
        writtenTop = element.scrollTop;
      }
    });
  };
  const onScroll = () => {
    const resized = clientHeight !== element.clientHeight || scrollHeight !== element.scrollHeight;
    clientHeight = element.clientHeight;
    scrollHeight = element.scrollHeight;
    // Composer growth and UI scale changes can clamp the offset. That is not
    // a reader scrolling away, and must not hide the pending status row.
    if (resized) { if (following) schedule(); return; }
    if (element.scrollTop === writtenTop) return;
    writtenTop = undefined;
    following = scrollHeight - clientHeight - element.scrollTop <= 32;
  };
  element.addEventListener("scroll", onScroll, { passive: true });
  const mutations = new MutationObserver(schedule);
  mutations.observe(element, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["open"] });
  const resize = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(schedule);
  resize?.observe(element);
  return {
    resume() { following = true; schedule(); },
    dispose() {
      disposed = true;
      mutations.disconnect();
      resize?.disconnect();
      element.removeEventListener("scroll", onScroll);
    },
  };
}
