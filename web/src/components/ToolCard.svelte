<script>
  import { approve } from "../lib/chat.svelte.js";

  // `tool` is a reactive transcript item: {data, open, touched}; data is the latest tool event.
  let { tool } = $props();
  let decided = $state(false);

  const data = $derived(tool.data);
  const lines = $derived((data.title || data.name || "").split("\n"));
  const title = $derived(lines[0]);
  const rest = $derived(lines.slice(1));
  const chip = $derived(
    { pending: "needs approval", running: "running", done: "done", failed: "failed", denied: "denied", cancelled: "cancelled" }[data.status] || data.status,
  );
  // The title can be shortened by the tool (a script shows only its first line), so show
  // every argument as the model sent it.
  const args = $derived(data.args && typeof data.args === "object" ? Object.entries(data.args) : []);

  function decide(allow) {
    decided = true;
    approve(data.id, allow);
  }
</script>

<details class="tool" data-status={data.status} open={tool.open}>
  <summary onclick={(e) => { e.preventDefault(); tool.touched = true; tool.open = !tool.open; }}>
    <span class="kind">{data.kind || "tool"}</span>
    <span class="title" title={title}>{title}</span>
    <span class="chip" class:spin={data.status === "running"}><span>{chip}</span></span>
  </summary>
  <div class="body">
    {#each args as [key, value]}
      <div class="label">{key}</div>
      <pre>{typeof value === "string" ? value : JSON.stringify(value)}</pre>
    {:else}
      {#if rest.length}
        <div class="label">call</div>
        <pre>{rest.join("\n")}</pre>
      {/if}
    {/each}
    {#if data.output}
      <div class="label">output</div>
      <pre>{data.output}</pre>
    {/if}
    {#if data.status === "pending"}
      <div class="actions">
        <button class="btn primary" disabled={decided} onclick={() => decide(true)}>Allow</button>
        <button class="btn" disabled={decided} onclick={() => decide(false)}>Deny</button>
      </div>
    {/if}
  </div>
</details>

<style>
  .tool { border: 1px solid var(--border); border-radius: 10px; background: var(--surface); font-size: 13px; }
  summary { display: flex; align-items: center; gap: 10px; padding: 8px 12px; cursor: pointer; list-style: none; }
  summary::-webkit-details-marker { display: none; }
  summary::before { content: "▸"; color: var(--muted); font-size: 11px; }
  .tool[open] > summary::before { content: "▾"; }
  .kind { font: 11px var(--mono); text-transform: uppercase; letter-spacing: .04em; color: var(--muted); border: 1px solid var(--border); border-radius: 4px; padding: 0 5px; }
  .title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); }
  .chip { font-size: 12px; color: var(--muted); display: flex; align-items: center; gap: 6px; }
  .tool[data-status=done] .chip { color: var(--ok); }
  .tool[data-status=failed] .chip, .tool[data-status=denied] .chip { color: var(--bad); }
  .tool[data-status=pending] { border-color: var(--warn); }
  .tool[data-status=pending] .chip { color: var(--warn); }
  .spin::before { content: ""; width: 10px; height: 10px; border: 2px solid var(--border); border-top-color: var(--accent); border-radius: 50%; animation: spin .8s linear infinite; }
  .body { padding: 0 12px 12px; display: flex; flex-direction: column; gap: 8px; }
  .body pre { max-height: 520px; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; }
  .label { color: var(--muted); font-size: 12px; margin-bottom: -4px; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
</style>
