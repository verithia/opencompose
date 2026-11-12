/** Follow new content only while the reader remains near the transcript bottom. */
export function followTranscript(element: HTMLElement) {
  let following = true;
  let queued = false;
  let disposed = false;
  const schedule = () => {
    if (queued || disposed) return;
    queued = true;
    queueMicrotask(() => {
      queued = false;
      if (!disposed && following && element.isConnected) element.scrollTop = element.scrollHeight;
    });
  };
  const onScroll = () => {
    following = element.scrollHeight - element.clientHeight - element.scrollTop <= 32;
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
