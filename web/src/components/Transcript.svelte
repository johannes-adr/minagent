<script>
  import { follow } from "../lib/follow.js";
  import Feed from "./Feed.svelte";

  let { pane } = $props();
</script>

<div class="scroll" use:follow>
  <div class="chat">
    {#if !pane.items.length}
      <div class="empty"><b>How can I help?</b>Ask something, or give the agent a task.</div>
    {/if}
    <Feed {pane} />
  </div>
</div>

<style>
  .scroll { flex: 1; min-width: 0; overflow-y: auto; }
  .chat { max-width: 800px; margin: 0 auto; padding: 24px 20px 12px; display: flex; flex-direction: column; gap: 16px; }
  .empty { color: var(--muted); text-align: center; margin-top: 18vh; }
  .empty b { display: block; font-size: 18px; color: var(--text); margin-bottom: 4px; }
</style>
