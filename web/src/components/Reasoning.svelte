<script>
  // `item` is a reactive transcript item: {text, live, open, touched}.
  let { item } = $props();
  let body;

  $effect(() => {
    item.text;
    if (body) body.scrollTop = body.scrollHeight;
  });
</script>

<details class="reasoning" open={item.open}>
  <summary
    onclick={(e) => { e.preventDefault(); item.touched = true; item.open = !item.open; }}
  >Thinking{item.live ? " …" : ""}</summary>
  <div bind:this={body}>{item.text}</div>
</details>

<style>
  .reasoning { border: 1px solid var(--border); border-radius: 10px; font-size: 13px; color: var(--muted); }
  summary { cursor: pointer; padding: 6px 12px; list-style: none; }
  summary::-webkit-details-marker { display: none; }
  summary::before { content: "▸ "; font-size: 11px; }
  details[open] > summary::before { content: "▾ "; }
  div { padding: 0 12px 10px; white-space: pre-wrap; overflow-wrap: anywhere; max-height: 360px; overflow-y: auto; font-style: italic; }
</style>
