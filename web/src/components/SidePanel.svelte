<script>
  import { chat } from "../lib/chat.svelte.js";
  import { follow } from "../lib/follow.js";
  import Feed from "./Feed.svelte";
</script>

<aside>
  <div class="head"><span>Subagents</span><button title="Hide" onclick={() => (chat.sideHidden = true)}>✕</button></div>
  <div class="tabs">
    {#each chat.subs as s (s.id)}
      <button class="tab" class:active={s.id === chat.activeSub} data-status={s.status} title={s.description} onclick={() => (chat.activeSub = s.id)}>
        <i></i><span class="t">{s.description}</span>
      </button>
    {/each}
  </div>
  <div class="panes">
    {#each chat.subs as s (s.id)}
      {#if s.id === chat.activeSub}
        <div class="pane" use:follow>
          <details class="task">
            <summary>{s.description}</summary>
            <pre>{s.prompt}</pre>
          </details>
          <div class="chat"><Feed pane={s.pane} /></div>
        </div>
      {/if}
    {/each}
  </div>
</aside>

<style>
  aside { display: flex; flex-direction: column; width: min(460px, 45vw); border-left: 1px solid var(--border); background: var(--surface); min-height: 0; }
  @media (max-width: 800px) { aside { width: auto; height: 50%; border-left: 0; border-top: 1px solid var(--border); } }
  .head { display: flex; align-items: center; justify-content: space-between; padding: 8px 12px; border-bottom: 1px solid var(--border); font-weight: 600; font-size: 13px; }
  .head button { border: 0; background: none; color: var(--muted); padding: 2px 6px; }
  .tabs { display: flex; gap: 4px; padding: 6px 8px 0; overflow-x: auto; border-bottom: 1px solid var(--border); }
  .tab { display: flex; align-items: center; gap: 6px; max-width: 180px; padding: 5px 10px; border: 1px solid transparent; border-bottom: 0; border-radius: 8px 8px 0 0; background: none; color: var(--muted); font-size: 12px; }
  .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tab.active { background: var(--bg); border-color: var(--border); color: var(--text); margin-bottom: -1px; }
  i { flex: none; width: 8px; height: 8px; border-radius: 50%; background: var(--accent); animation: pulse 1.1s ease-in-out infinite; }
  .tab[data-status=done] i { background: var(--ok); animation: none; }
  .tab[data-status=failed] i, .tab[data-status=cancelled] i { background: var(--bad); animation: none; }
  .panes { flex: 1; min-height: 0; position: relative; background: var(--bg); }
  .pane { position: absolute; inset: 0; overflow-y: auto; padding: 14px 14px 20px; }
  .chat { display: flex; flex-direction: column; gap: 12px; margin-top: 12px; }
  .task { font-size: 13px; color: var(--muted); }
  .task summary { cursor: pointer; color: var(--text); font-weight: 600; }
  .task pre { margin-top: 6px; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 12px; max-height: 240px; overflow: auto; }
</style>
