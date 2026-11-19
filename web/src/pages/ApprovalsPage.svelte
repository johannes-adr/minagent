<script>
  import { onMount } from "svelte";
  import { chat, loadApprovals, openSession } from "../lib/chat.svelte.js";
  import { go } from "../lib/route.svelte.js";
  import { decide, fenced } from "../lib/approvals.js";
  import Markdown from "../components/Markdown.svelte";

  let comments = $state({}); // session -> comment text
  let sending = $state({}); // session -> true while a decision is on its way
  let error = $state("");

  onMount(loadApprovals);

  async function send(approval, approve) {
    error = "";
    sending[approval.session] = true;
    try {
      await decide(approval.session, approve, comments[approval.session]);
    } catch (e) {
      error = e.message;
    }
    sending[approval.session] = false;
    await loadApprovals();
  }

  async function open(session) {
    go("/");
    await openSession(session);
  }
</script>

<header>
  <button class="btn" onclick={() => go("/")}>← Chat</button>
  <h1>Approvals</h1>
  <span class="count">{chat.approvals.length} waiting</span>
</header>

<div class="scroll">
  <div class="page">
    {#if error}<div class="error">{error}</div>{/if}
    {#each chat.approvals as approval (approval.session)}
      <div class="card">
        <div class="meta">
          <span class="hook">{approval.hook}</span>
          <span>{new Date(approval.time * 1000).toLocaleString()}</span>
          <button class="link" onclick={() => open(approval.session)}>Open session</button>
        </div>
        {#if approval.message}
          <div class="message"><Markdown source={approval.message} /></div>
        {/if}
        <div class="call">
          <div class="title">
            The run wants to change data{#if approval.hosts?.length} on <code>{approval.hosts.join(", ")}</code>{/if}.
            It stays paused until you decide.
          </div>
          <details>
            <summary>Script</summary>
            <Markdown source={fenced(approval.code)} />
          </details>
        </div>
        <textarea
          rows="2" placeholder="Comment for the agent (optional; sent with your decision)"
          bind:value={comments[approval.session]}
        ></textarea>
        <div class="actions">
          <button class="btn primary" disabled={sending[approval.session]} onclick={() => send(approval, true)}>Approve</button>
          <button class="btn" disabled={sending[approval.session]} onclick={() => send(approval, false)}>
            {comments[approval.session]?.trim() ? "Decline with comment" : "Decline"}
          </button>
        </div>
      </div>
    {:else}
      <div class="empty">Nothing is waiting for you.</div>
    {/each}
  </div>
</div>

<style>
  header { display: flex; align-items: center; gap: 12px; padding: 12px 20px; border-bottom: 1px solid var(--border); }
  h1 { margin: 0; font-size: 15px; font-weight: 600; }
  .count { color: var(--muted); font-size: 12px; }
  .scroll { flex: 1; min-height: 0; overflow-y: auto; }
  .page { max-width: 800px; margin: 0 auto; padding: 24px 20px 60px; display: flex; flex-direction: column; gap: 16px; }
  .empty { color: var(--muted); text-align: center; margin-top: 18vh; }
  .card { border: 1px solid var(--warn); border-radius: 10px; background: var(--surface); padding: 12px 14px; display: flex; flex-direction: column; gap: 10px; font-size: 14px; }
  .meta { display: flex; align-items: center; gap: 12px; color: var(--muted); font-size: 12px; }
  .hook { font: 13px var(--mono); color: var(--text); }
  .link { margin-left: auto; border: 0; background: none; color: var(--accent); padding: 0; }
  .message { border-left: 3px solid var(--border); padding-left: 12px; }
  .call { display: flex; flex-direction: column; gap: 6px; }
  summary { cursor: pointer; color: var(--muted); font-size: 13px; }
  textarea { padding: 6px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; resize: vertical; }
  textarea:focus { outline: 0; border-color: var(--accent); }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .error { border: 1px solid var(--bad); color: var(--bad); border-radius: 8px; padding: 8px 12px; overflow-wrap: anywhere; }
</style>
