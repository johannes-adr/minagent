<script>
  import { chat, loadSessions, setThinking } from "../lib/chat.svelte.js";
  import { go } from "../lib/route.svelte.js";
  import ToolsMenu from "./ToolsMenu.svelte";

  // While dragging only the label follows; the level is sent on release. Each level sent
  // while dragging came back from the server and moved the slider under the finger.
  let dragging = $state(false);
  let dragIndex = $state(0);

  const levels = $derived(chat.thinking?.levels ?? []);
  const index = $derived(Math.max(0, levels.indexOf(chat.thinking?.current)));
  const shown = $derived(dragging ? dragIndex : index);
  const label = $derived.by(() => {
    if (!chat.thinking) return "";
    if (dragging) return levels[dragIndex];
    const { current, default: fallback } = chat.thinking;
    return current + (current === fallback ? " (default)" : "");
  });

  const context = $derived(chat.context);
  const percent = $derived(context?.percent ?? null);
  const contextTitle = $derived(
    !context ? "" : context.window
      ? `${context.tokens.toLocaleString()} of ${context.window.toLocaleString()} tokens of the context window in use`
      : `${context.tokens.toLocaleString()} tokens in the context (window size unknown: set AGENT_CONTEXT_WINDOW)`,
  );

  async function release(e) {
    await setThinking(levels[e.target.value]);
    dragging = false;
  }
</script>

<header>
  <button class="btn sessions" onclick={() => { loadSessions(); chat.drawer = true; }}>☰ Sessions</button>
  <h1>agent</h1>
  {#if chat.thinking}
    <div class="thinking" title="How much the model thinks before it answers">
      <span>Thinking</span>
      <input
        type="range" min="0" max={levels.length - 1} step="1" value={shown}
        oninput={(e) => { dragging = true; dragIndex = +e.target.value; }}
        onchange={release}
      />
      <span class="level">{label}</span>
    </div>
  {/if}
  {#if context}
    <div class="context" class:nowindow={percent == null} class:warn={percent >= 80 && percent < 95} class:full={percent >= 95} title={contextTitle}>
      <span class="meter"><span style:width="{Math.min(percent ?? 0, 100)}%"></span></span>
      <span>{context.text}</span>
    </div>
  {/if}
  <ToolsMenu />
  <button class="btn settings" title="Files: skills and working directory" onclick={() => go("/files")}>🗂</button>
  <button class="btn settings" title="Webhooks" onclick={() => go("/hooks")}>⚡</button>
  <button class="btn settings" title="Settings" onclick={() => go("/settings")}>⚙</button>
  <div class="state" class:busy={chat.busy} class:offline={chat.offline}>
    <span class="dot"></span><span>{chat.stateText}</span>
  </div>
</header>

<style>
  header { display: flex; align-items: center; gap: 10px; padding: 12px 20px; border-bottom: 1px solid var(--border); }
  .sessions { padding: 4px 10px; font-size: 13px; }
  .settings { padding: 3px 9px; font-size: 14px; }
  h1 { margin: 0 auto 0 0; font-size: 15px; font-weight: 600; }
  .context { display: flex; align-items: center; gap: 8px; margin-right: 8px; color: var(--muted); font-size: 13px; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .meter { width: 64px; height: 6px; border-radius: 3px; background: var(--border); overflow: hidden; }
  .meter > span { display: block; height: 100%; background: var(--muted); transition: width .3s; }
  .nowindow .meter { display: none; }
  .warn { color: var(--warn); } .warn .meter > span { background: var(--warn); }
  .full { color: var(--bad); } .full .meter > span { background: var(--bad); }
  .thinking { display: flex; align-items: center; gap: 8px; margin-right: 8px; color: var(--muted); font-size: 13px; white-space: nowrap; }
  .thinking input { width: 96px; accent-color: var(--accent); }
  .level { min-width: 4.5em; }
  @media (max-width: 480px) { .thinking input { width: 64px; } .meter { display: none; } }
  .state { display: flex; align-items: center; gap: 8px; color: var(--muted); font-size: 13px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--ok); }
  .busy .dot { background: var(--accent); animation: pulse 1.1s ease-in-out infinite; }
  .offline .dot { background: var(--bad); }
</style>
