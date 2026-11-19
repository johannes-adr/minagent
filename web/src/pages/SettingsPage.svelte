<script>
  import { onMount } from "svelte";
  import { chat } from "../lib/chat.svelte.js";
  import { get, put } from "../lib/api.js";
  import { go } from "../lib/route.svelte.js";
  import SettingField from "../components/SettingField.svelte";

  let config = $state(null);
  let draft = $state({}); // name -> edited value; `null` removes the setting from the file
  let error = $state("");
  let saved = $state(false);
  let saving = $state(false);
  let secretName = $state("");
  let secretValue = $state("");

  const GROUPS = [
    ["Model", (n) => n.startsWith("LLM_") || n === "AGENT_CONTEXT_WINDOW"],
    ["Providers", (n) => /^(OPENAI|ANTHROPIC|OLLAMA)_/.test(n)],
    ["Tools and skills", (n) => n === "AGENT_TOOLS" || n === "AGENT_DISABLED_SKILLS"],
    ["execute_javascript", (n) => n.startsWith("AGENT_JS_")],
    ["Secrets", (n) => n.startsWith("AGENT_SECRET_")],
  ];

  const groups = $derived(
    config
      ? GROUPS.map(([title, test]) => [title, config.entries.filter((e) => test(e.name))]).filter(([, e]) => e.length)
      : [],
  );
  const changed = $derived(Object.keys(draft).length > 0);

  async function load() {
    const response = await get("/config");
    if (response.ok) config = await response.json();
    else error = (await response.json().catch(() => ({}))).error || `HTTP ${response.status}`;
  }
  onMount(load);

  const shown = (entry) => (entry.name in draft ? (draft[entry.name] ?? "") : entry.value);
  const edit = (name, value) => { draft[name] = value; saved = false; };
  const reset = (name) => { draft[name] = null; saved = false; };

  async function save() {
    saving = true; error = ""; saved = false;
    try {
      const response = await put("/config", draft);
      const body = await response.json().catch(() => ({}));
      if (response.ok) {
        config = body; draft = {}; saved = true;
      } else {
        error = body.error || `HTTP ${response.status}`;
      }
    } finally {
      saving = false;
    }
  }

  function addSecret() {
    const name = secretName.trim().toUpperCase().replace(/[^A-Z0-9_]/g, "_");
    if (!name || !secretValue.trim()) return;
    edit("AGENT_SECRET_" + name, secretValue.trim());
    // Shown in the list right away; it is only saved with the rest.
    config.entries.push({ name: "AGENT_SECRET_" + name, doc: "execute_javascript secret as `<host> <Authorization header value>`", kind: "secret-host", custom: true, source: "default", set: false, value: "", hint: null });
    secretName = ""; secretValue = "";
  }
  function deleteSecret(entry) {
    reset(entry.name);
    config.entries = config.entries.filter((e) => e.name !== entry.name);
  }
</script>

<header>
  <button class="btn" onclick={() => go("/")}>← Chat</button>
  <h1>Settings</h1>
  <span class="path">{config?.path ?? ""}</span>
  <span class="spacer"></span>
  {#if saved}<span class="ok">Saved and applied</span>{/if}
  <button class="btn primary" disabled={!changed || saving || chat.busy} onclick={save}>
    {saving ? "Saving…" : "Save"}
  </button>
</header>

<main>
  <div class="page">
    {#if chat.busy}<div class="note">The agent is working: settings can be saved once it has finished.</div>{/if}
    {#if error}<div class="error">{error}</div>{/if}
    {#if config?.problems.length}
      {#each config.problems as problem}<div class="warn">{problem}</div>{/each}
    {/if}
    <p class="intro">
      Saved to <code>{config?.path}</code> and applied to the next message. Values from the file win over
      the environment and <code>.env</code>. Tools chosen for a single chat (in the chat header) override the default here.
    </p>

    {#each groups as [title, entries] (title)}
      <section>
        <h2>{title}</h2>
        {#each entries as entry (entry.name)}
          <SettingField
            {entry} value={shown(entry)} tools={config.tools} skills={config.skills}
            onchange={(v) => edit(entry.name, v)} onreset={() => reset(entry.name)} ondelete={() => deleteSecret(entry)}
          />
        {/each}
        {#if title === "Secrets"}
          <form class="add" onsubmit={(e) => { e.preventDefault(); addSecret(); }}>
            <input placeholder="NAME (e.g. GITLAB)" bind:value={secretName} />
            <input type="password" autocomplete="off" placeholder="<host> <header value>" bind:value={secretValue} />
            <button class="btn">Add secret</button>
          </form>
        {/if}
      </section>
    {/each}
    {#if !config && !error}<div class="note">Loading…</div>{/if}
  </div>
</main>

<style>
  header { display: flex; align-items: center; gap: 12px; padding: 12px 20px; border-bottom: 1px solid var(--border); }
  h1 { margin: 0; font-size: 15px; font-weight: 600; }
  header .btn { white-space: nowrap; }
  .path { color: var(--muted); font: 12px var(--mono); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .spacer { flex: 1; }
  .ok { color: var(--ok); font-size: 13px; }
  main { flex: 1; min-height: 0; overflow-y: auto; }
  .page { max-width: 760px; margin: 0 auto; padding: 8px 20px 60px; }
  .intro { color: var(--muted); font-size: 13px; }
  h2 { margin: 28px 0 0; font-size: 14px; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); }
  .note { color: var(--muted); font-size: 13px; margin-top: 12px; }
  .error, .warn { border-radius: 8px; padding: 8px 12px; margin-top: 12px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .error { border: 1px solid var(--bad); color: var(--bad); }
  .warn { border: 1px solid var(--warn); color: var(--warn); }
  .add { display: flex; gap: 8px; margin-top: 14px; flex-wrap: wrap; }
  .add input { padding: 6px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; flex: 1; min-width: 160px; }
</style>
