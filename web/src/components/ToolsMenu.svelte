<script>
  import { chat, setTools } from "../lib/chat.svelte.js";

  let open = $state(false);
  const tools = $derived(chat.tools);

  function toggle(name, on) {
    // Keep the order of the available tools, whatever the order of clicks.
    const next = tools.available.filter((t) => (t === name ? on : tools.enabled.includes(t)));
    setTools(next);
  }
</script>

{#if tools}
  <div class="menu">
    <button class="btn trigger" class:custom={tools.custom} title="Tools of this chat" onclick={() => (open = !open)}>
      Tools {tools.enabled.length}/{tools.available.length}
    </button>
    {#if open}
      <div class="scrim" onclick={() => (open = false)} role="presentation"></div>
      <div class="popover">
        <div class="title">Tools for this chat</div>
        {#each tools.available as name}
          <label>
            <input
              type="checkbox" checked={tools.enabled.includes(name)} disabled={chat.busy}
              onchange={(e) => toggle(name, e.target.checked)}
            />
            <code>{name}</code>
          </label>
        {/each}
        <div class="foot">
          {#if tools.custom}
            <span>differs from the default</span>
            <button class="btn" disabled={chat.busy} onclick={() => setTools(null)}>Reset</button>
          {:else}
            <span>default from the settings</span>
          {/if}
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .menu { position: relative; margin-right: 8px; }
  .trigger { padding: 3px 10px; font-size: 13px; }
  .trigger.custom { border-color: var(--accent); color: var(--accent); }
  .scrim { position: fixed; inset: 0; z-index: 4; }
  .popover { position: absolute; right: 0; top: calc(100% + 6px); z-index: 5; min-width: 230px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; background: var(--surface); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 6px 24px rgba(0,0,0,.25); }
  .title { font-weight: 600; font-size: 13px; }
  label { display: flex; align-items: center; gap: 8px; font-size: 13px; cursor: pointer; }
  .foot { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 4px; padding-top: 8px; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px; }
  .foot .btn { padding: 2px 10px; font-size: 12px; }
</style>
