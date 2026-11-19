<script>
  import { markdown } from "../lib/markdown.js";

  let { source } = $props();
  const html = $derived(markdown(source));

  function copy(e) {
    if (!e.target.classList.contains("copy")) return;
    navigator.clipboard?.writeText(e.target.parentElement.querySelector("pre").innerText);
    e.target.textContent = "Copied";
    setTimeout(() => (e.target.textContent = "Copy"), 1200);
  }
</script>

<!-- The renderer escapes all text and only links http(s)/mailto, so {@html} is safe here. -->
<div class="md" onclick={copy} role="presentation">{@html html}</div>

<style>
  .md { overflow-wrap: anywhere; }
  .md > :global(:first-child) { margin-top: 0; }
  .md > :global(:last-child) { margin-bottom: 0; }
  .md :global(h1), .md :global(h2), .md :global(h3), .md :global(h4), .md :global(h5), .md :global(h6) { margin: 1.1em 0 .4em; line-height: 1.3; }
  .md :global(h1) { font-size: 1.35em; } .md :global(h2) { font-size: 1.2em; }
  .md :global(h3), .md :global(h4) { font-size: 1.05em; } .md :global(h5), .md :global(h6) { font-size: 1em; }
  .md :global(h6) { color: var(--muted); }
  .md :global(p) { margin: .6em 0; }
  .md :global(ul), .md :global(ol) { margin: .6em 0; padding-left: 1.5em; }
  .md :global(li > ul), .md :global(li > ol), .md :global(li > .code), .md :global(li > p) { margin: .25em 0; }
  .md :global(li.task) { list-style: none; margin-left: -1.2em; }
  .md :global(li.task input) { margin-right: 6px; }
  .md :global(del) { color: var(--muted); }
  .md :global(.table-wrap) { overflow-x: auto; margin: .7em 0; }
  .md :global(table) { border-collapse: collapse; font-size: 14px; }
  .md :global(th), .md :global(td) { border: 1px solid var(--border); padding: 5px 10px; vertical-align: top; }
  .md :global(th) { background: var(--surface); text-align: left; }
  .md :global(.lang) { position: absolute; top: 6px; left: 12px; font: 11px var(--mono); color: var(--muted); }
  .md :global(.code:has(.lang) pre) { padding-top: 26px; }
  .md :global(blockquote) { margin: .6em 0; padding: 0 12px; border-left: 3px solid var(--border); color: var(--muted); }
  .md :global(hr) { border: 0; border-top: 1px solid var(--border); }
  .md :global(a) { color: var(--accent); }
  .md :global(.code) { position: relative; margin: .7em 0; }
  .md :global(.copy) { position: absolute; top: 6px; right: 6px; font: 12px inherit; padding: 2px 8px; border: 1px solid var(--border); border-radius: 6px; background: var(--surface); color: var(--muted); cursor: pointer; opacity: 0; transition: opacity .15s; }
  .md :global(.code:hover .copy) { opacity: 1; }
</style>
