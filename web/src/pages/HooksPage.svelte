<script>
  import { onMount } from "svelte";
  import { go } from "../lib/route.svelte.js";
  import * as api from "../lib/hooks.js";

  let info = $state(null); // {hooks, isolated, tools, queued, max_queue}
  let selected = $state(null); // name of the hook being edited; null with `form` set: a new one
  let form = $state(null);
  let error = $state("");
  let saved = $state(false);
  let secret = $state(null); // {name, token}: shown once
  let copied = $state(false);

  const hooks = $derived(info?.hooks ?? []);

  async function load() {
    try { info = await api.list(); } catch (e) { error = e.message; }
  }
  onMount(load);

  function pick(hook) {
    selected = hook.name; form = api.toForm(hook); error = ""; saved = false; secret = null;
  }
  function create() {
    selected = null; form = api.blank(); error = ""; saved = false; secret = null;
  }

  async function save() {
    error = ""; saved = false;
    try {
      const result = await api.save(api.toBody(form));
      await load();
      if (result.token) secret = { name: result.hook.name, token: result.token };
      selected = result.hook.name;
      form = api.toForm(result.hook);
      saved = true;
    } catch (e) {
      error = e.message;
    }
  }

  async function renew() {
    if (!confirm(`Create a new token for ${form.name}? The current one stops working.`)) return;
    try { secret = { name: form.name, token: (await api.renewToken(form.name)).token }; } catch (e) { error = e.message; }
  }

  async function remove() {
    if (!confirm(`Delete the hook ${form.name}?`)) return;
    try { await api.remove(form.name); form = null; selected = null; secret = null; await load(); } catch (e) { error = e.message; }
  }

  function toggleTool(name, on) {
    form.tools = on ? [...form.tools, name] : form.tools.filter((t) => t !== name);
  }
  function toggleSkill(name, on) {
    form.skills = on ? [...form.skills, name] : form.skills.filter((t) => t !== name);
  }
  // Skills the hook lists that no longer exist stay visible, so they can be removed.
  const skillNames = $derived([...new Set([...(info?.skills ?? []).map((s) => s.name), ...(form?.skills ?? [])])].sort());

  async function copy(text) {
    await navigator.clipboard?.writeText(text);
    copied = true;
    setTimeout(() => (copied = false), 1200);
  }
</script>

<header>
  <button class="btn" onclick={() => go("/")}>← Chat</button>
  <h1>Webhooks</h1>
  {#if info}<span class="queue">{info.queued} waiting (max {info.max_queue})</span>{/if}
</header>

<div class="body">
  <aside>
    <div class="bar"><button class="btn primary" onclick={create}>+ New hook</button></div>
    <ul>
      {#each hooks as hook (hook.name)}
        <li class:current={selected === hook.name}>
          <button onclick={() => pick(hook)}>
            <span class="dot" class:off={!hook.enabled}></span>
            <span class="name">{hook.name}</span>
          </button>
        </li>
      {:else}
        <li class="empty">No hooks yet.</li>
      {/each}
    </ul>
    <p class="help">
      A hook lets an outside service start the agent with <code>POST /hooks/&lt;name&gt;</code>. Each run is a
      session of its own, one after the other. Nobody can answer questions during a run, so whatever would ask
      is declined unless it is allowed here.
    </p>
  </aside>

  <section>
    {#if !form}
      <div class="placeholder">Select a hook or create a new one.</div>
    {:else}
      {#if secret}
        <div class="secret">
          <strong>Token for {secret.name}: copy it now, it is not shown again.</strong>
          <div class="token"><code>{secret.token}</code><button class="btn" onclick={() => copy(secret.token)}>{copied ? "Copied" : "Copy"}</button></div>
          <pre>{api.curl(secret.name, secret.token)}</pre>
        </div>
      {/if}
      {#if error}<div class="error">{error}</div>{/if}

      <div class="form">
        <label class="row">
          <span>Name</span>
          <input bind:value={form.name} disabled={!form.create} placeholder="github-issues" />
        </label>
        {#if !form.create}
          <div class="hint">URL: <code>{api.urlOf(form.name)}</code> · token in <code>Authorization: Bearer …</code>, <code>X-Hook-Token</code> or <code>?token=</code></div>
        {/if}
        <label class="check"><input type="checkbox" bind:checked={form.enabled} /> Enabled</label>

        <label class="col">
          <span>Prompt</span>
          <textarea bind:value={form.template} rows="6" spellcheck="false" placeholder={"New issue: {{body.issue.title}}\n\n{{body.issue.body}}"}></textarea>
          <small>
            Placeholders: <code>{"{{payload}}"}</code> (raw body), <code>{"{{body.a.b.0}}"}</code> (JSON field),
            <code>{"{{headers.x-github-event}}"}</code>, <code>{"{{hook}}"}</code>. Anything sent is untrusted text.
          </small>
        </label>

        <div class="col">
          <span>Tools of a run (only these exist)</span>
          <div class="tools">
            {#each info?.tools ?? [] as name}
              {@const blocked = name === "execute" && !info.isolated}
              <label class="check" title={blocked ? "A shell is only available to hooks inside an isolated container (AGENT_ISOLATED=1)" : ""}>
                <input type="checkbox" disabled={blocked} checked={form.tools.includes(name)} onchange={(e) => toggleTool(name, e.target.checked)} />
                <code>{name}</code>{#if blocked}<small>(container only)</small>{/if}
              </label>
            {/each}
          </div>
        </div>

        <div class="col">
          <span>Skills of a run (none by default)</span>
          <div class="tools">
            {#each skillNames as name}
              {@const known = (info?.skills ?? []).find((s) => s.name === name)}
              <label class="check" title={known?.description ?? "this skill no longer exists"}>
                <input type="checkbox" checked={form.skills.includes(name)} onchange={(e) => toggleSkill(name, e.target.checked)} />
                <code>{name}</code>
              </label>
            {:else}
              <small>No skills yet.</small>
            {/each}
          </div>
        </div>

        <label class="col">
          <span>Hosts scripts may reach (one per line)</span>
          <textarea bind:value={form.hosts} rows="3" spellcheck="false" placeholder="api.github.com"></textarea>
          <small>Replaces <code>AGENT_JS_ALLOWED_HOSTS</code> for runs of this hook. Everything else is declined.</small>
        </label>
        <label class="check"><input type="checkbox" bind:checked={form.allow_writes} /> Scripts may change data (POST, PUT, PATCH, DELETE) on those hosts</label>

        <label class="row">
          <span>Callback URL</span>
          <input bind:value={form.callback_url} placeholder="https://example.com/agent-result (optional)" />
        </label>
        <label class="row">
          <span>Time limit (minutes)</span>
          <input type="number" min="1" max="240" bind:value={form.timeout_minutes} class="short" />
        </label>

        <div class="actions">
          <button class="btn primary" onclick={save}>{form.create ? "Create hook" : "Save"}</button>
          {#if saved}<span class="ok">Saved</span>{/if}
          <span class="spacer"></span>
          {#if !form.create}
            <button class="btn" onclick={renew}>New token</button>
            <button class="btn danger" onclick={remove}>Delete</button>
          {/if}
        </div>
      </div>
    {/if}
  </section>
</div>

<style>
  header { display: flex; align-items: center; gap: 12px; padding: 12px 20px; border-bottom: 1px solid var(--border); }
  h1 { margin: 0; font-size: 15px; font-weight: 600; }
  header .btn { white-space: nowrap; }
  .queue { color: var(--muted); font-size: 12px; }
  .body { flex: 1; min-height: 0; display: flex; }
  aside { width: min(300px, 38vw); display: flex; flex-direction: column; border-right: 1px solid var(--border); background: var(--surface); min-height: 0; }
  .bar { padding: 10px 12px; border-bottom: 1px solid var(--border); }
  ul { list-style: none; margin: 0; padding: 4px; flex: 1; overflow-y: auto; }
  li button { width: 100%; display: flex; align-items: center; gap: 8px; padding: 6px 10px; border: 0; border-radius: 8px; background: none; color: var(--text); text-align: left; }
  li:hover button { background: var(--code); } li.current button { background: var(--user); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--ok); } .dot.off { background: var(--muted); }
  .name { font-family: var(--mono); font-size: 13px; }
  .empty { color: var(--muted); padding: 8px; font-size: 13px; }
  .help { margin: 0; padding: 12px; color: var(--muted); font-size: 12px; border-top: 1px solid var(--border); }
  section { flex: 1; min-width: 0; overflow-y: auto; padding: 16px 24px 60px; }
  .placeholder { color: var(--muted); text-align: center; margin-top: 18vh; }
  .form { display: flex; flex-direction: column; gap: 14px; max-width: 720px; }
  .row { display: flex; align-items: center; gap: 12px; } .row > span { width: 150px; flex: none; color: var(--muted); font-size: 13px; }
  .col { display: flex; flex-direction: column; gap: 4px; } .col > span { color: var(--muted); font-size: 13px; }
  input, textarea { padding: 6px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; }
  .row input { flex: 1; min-width: 0; } .short { max-width: 100px; flex: none !important; }
  textarea { font: 13px/1.5 var(--mono); resize: vertical; }
  input:focus, textarea:focus { outline: 0; border-color: var(--accent); }
  small, .hint { color: var(--muted); font-size: 12px; }
  .check { display: flex; align-items: center; gap: 8px; font-size: 14px; }
  .tools { display: flex; flex-wrap: wrap; gap: 6px 18px; }
  .actions { display: flex; align-items: center; gap: 10px; margin-top: 6px; } .spacer { flex: 1; }
  .ok { color: var(--ok); font-size: 13px; }
  .danger:hover { border-color: var(--bad); color: var(--bad); }
  .error { border: 1px solid var(--bad); color: var(--bad); border-radius: 8px; padding: 8px 12px; margin-bottom: 14px; overflow-wrap: anywhere; }
  .secret { border: 1px solid var(--warn); border-radius: 10px; padding: 12px; margin-bottom: 16px; display: flex; flex-direction: column; gap: 8px; max-width: 720px; }
  .token { display: flex; align-items: center; gap: 10px; } .token code { flex: 1; overflow-wrap: anywhere; }
  @media (max-width: 800px) { .body { flex-direction: column; } aside { width: auto; max-height: 40%; border-right: 0; border-bottom: 1px solid var(--border); } .row { flex-direction: column; align-items: stretch; gap: 4px; } }
</style>
