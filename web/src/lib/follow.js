// Svelte action: keeps a scrolling element at its bottom while content is added, unless
// the user has scrolled up to read something.
export function follow(node) {
  let pinned = true;
  const onScroll = () => {
    pinned = node.scrollHeight - node.scrollTop - node.clientHeight < 80;
  };
  let queued = false;
  const observer = new MutationObserver(() => {
    if (!pinned || queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      node.scrollTop = node.scrollHeight;
    });
  });
  node.addEventListener("scroll", onScroll);
  observer.observe(node, { childList: true, subtree: true, characterData: true });
  return {
    destroy() {
      node.removeEventListener("scroll", onScroll);
      observer.disconnect();
    },
  };
}
