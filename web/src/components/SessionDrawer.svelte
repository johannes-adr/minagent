<script>
  import { chat, deleteSession, newSession, openSession } from "../lib/chat.svelte.js";
</script>

{#if chat.drawer}
  <div class="scrim" onclick={() => (chat.drawer = false)} role="presentation"></div>
{/if}
<aside class:open={chat.drawer}>
  <div class="head"><span>Sessions</span><button class="btn primary" onclick={newSession}>+ New</button></div>
  <div class="list">
    {#each chat.sessions as s (s.id)}
      <div class="session" class:current={s.id === chat.current} onclick={() => openSession(s.id)} role="presentation">
        <div class="name">
          {s.title || "(empty)"}
          <span class="when">{new Date(s.updated * 1000).toLocaleString()}</span>
        </div>
        <button class="del" title="Delete" onclick={(e) => { e.stopPropagation(); deleteSession(s.id); }}>✕</button>
      </div>
    {:else}
      <div class="none">No saved sessions yet.</div>
    {/each}
  </div>
</aside>

<style>
  aside { position: fixed; inset: 0 auto 0 0; width: min(320px, 85vw); z-index: 10; display: flex; flex-direction: column; background: var(--surface); border-right: 1px solid var(--border); transform: translateX(-100%); transition: transform .2s; }
  aside.open { transform: none; }
  .scrim { position: fixed; inset: 0; z-index: 9; background: rgba(0,0,0,.35); }
  .head { display: flex; align-items: center; justify-content: space-between; padding: 12px 14px; border-bottom: 1px solid var(--border); font-weight: 600; }
  .list { flex: 1; overflow-y: auto; padding: 8px; display: flex; flex-direction: column; gap: 2px; }
  .session { display: flex; align-items: center; gap: 6px; border-radius: 8px; padding: 6px 8px; cursor: pointer; }
  .session:hover { background: var(--code); }
  .session.current { background: var(--user); }
  .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .when { display: block; color: var(--muted); font-size: 11px; }
  .del { border: 0; background: none; color: var(--muted); padding: 2px 6px; border-radius: 6px; opacity: 0; }
  .session:hover .del { opacity: 1; }
  .del:hover { color: var(--bad); }
  .none { color: var(--muted); padding: 8px; font-size: 13px; }
</style>
