<script>
  import { answer } from "../lib/chat.svelte.js";

  // `item` is a reactive transcript item: {data, sent}; data is the latest question event.
  let { item } = $props();
  let text = $state("");

  const data = $derived(item.data);

  function send(value) {
    if (!value.trim()) return;
    item.sent = true;
    answer(data.id, value);
  }
</script>

<div class="question" data-status={data.status}>
  <div class="q">{data.question || ""}</div>
  {#if data.status === "answered"}
    <div class="answer">→ {data.answer}</div>
  {:else if data.status === "cancelled"}
    <div class="answer">not answered</div>
  {:else}
    {#if (data.options || []).length}
      <div class="actions">
        {#each data.options as option}
          <button class="btn" disabled={item.sent} onclick={() => send(option)}>{option}</button>
        {/each}
      </div>
    {/if}
    <form onsubmit={(e) => { e.preventDefault(); send(text); }}>
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={text} placeholder="Your answer…" disabled={item.sent} autofocus />
      <button class="btn primary" disabled={item.sent}>Send</button>
    </form>
  {/if}
</div>

<style>
  .question { border: 1px solid var(--accent); border-radius: 10px; background: var(--surface); padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; font-size: 14px; }
  .question[data-status=answered], .question[data-status=cancelled] { border-color: var(--border); }
  .q { font-weight: 600; white-space: pre-wrap; overflow-wrap: anywhere; }
  .answer { color: var(--muted); }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  form { display: flex; gap: 8px; }
  input { flex: 1; min-width: 0; padding: 5px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; }
</style>
