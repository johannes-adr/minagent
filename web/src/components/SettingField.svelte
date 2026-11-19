<script>
  // One setting. `entry` is what the server lists, `value` the draft (a string) and `tools`
  // the names of all tools; `onchange(value)` reports an edit, `onreset()` drops the value
  // from the settings file (the environment's, if any, applies again).
  let { entry, value, tools, onchange, onreset, ondelete } = $props();

  const kind = $derived(entry.kind);
  const choices = $derived(kind.startsWith("choice:") ? kind.slice(7).split(",") : []);
  const secret = $derived(kind.startsWith("secret"));
  const isOn = (v) => /^(1|true|yes)$/i.test(v.trim());
  const selected = $derived(
    value.trim() ? value.split(",").map((t) => t.trim()) : tools, // empty means all tools
  );

  function toggleTool(name, on) {
    const next = tools.filter((t) => (t === name ? on : selected.includes(t)));
    onchange(next.length === tools.length ? "" : next.join(","));
  }
</script>

<div class="field">
  <div class="top">
    <code class="name">{entry.name}</code>
    <span class="badge" data-source={entry.source}>{entry.source}</span>
    {#if secret && entry.set}<span class="badge set">set{entry.hint ? ` · ${entry.hint}` : ""}</span>{/if}
    <span class="spacer"></span>
    {#if entry.source === "file" || (secret && entry.set)}
      <button class="link" onclick={onreset} title="Remove from the settings file">
        {secret ? "Clear" : "Reset"}
      </button>
    {/if}
    {#if entry.custom}<button class="link bad" onclick={ondelete}>Delete</button>{/if}
  </div>
  <div class="doc">{entry.doc}</div>

  {#if choices.length}
    <select {value} onchange={(e) => onchange(e.target.value)}>
      <option value="">default</option>
      {#each choices as choice}<option value={choice}>{choice}</option>{/each}
    </select>
  {:else if kind === "bool"}
    <label class="check"><input type="checkbox" checked={isOn(value)} onchange={(e) => onchange(e.target.checked ? "1" : "")} /> enabled</label>
  {:else if kind === "tools"}
    <div class="tools">
      {#each tools as name}
        <label class="check">
          <input type="checkbox" checked={selected.includes(name)} onchange={(e) => toggleTool(name, e.target.checked)} />
          <code>{name}</code>
        </label>
      {/each}
    </div>
  {:else if secret}
    <input
      type="password" autocomplete="off" {value}
      placeholder={entry.set ? "unchanged (type to replace)" : kind === "secret-host" ? "<host> <header value>" : "not set"}
      oninput={(e) => onchange(e.target.value)}
    />
  {:else}
    <input type="text" {value} placeholder={entry.source === "default" ? "default" : ""} inputmode={kind === "number" ? "numeric" : undefined} oninput={(e) => onchange(e.target.value)} />
  {/if}
</div>

<style>
  .field { display: flex; flex-direction: column; gap: 4px; padding: 12px 0; border-bottom: 1px solid var(--border); }
  .top { display: flex; align-items: center; gap: 8px; }
  .spacer { flex: 1; }
  .name { font-weight: 600; }
  .badge { font-size: 11px; padding: 0 6px; border-radius: 4px; border: 1px solid var(--border); color: var(--muted); }
  .badge[data-source=file] { border-color: var(--accent); color: var(--accent); }
  .badge.set { color: var(--ok); border-color: var(--ok); }
  .doc { color: var(--muted); font-size: 13px; }
  input[type=text], input[type=password], select { padding: 6px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; max-width: 560px; }
  input:focus, select:focus { outline: 0; border-color: var(--accent); }
  .tools { display: flex; flex-wrap: wrap; gap: 6px 16px; }
  .check { display: flex; align-items: center; gap: 6px; font-size: 14px; cursor: pointer; }
  .link { border: 0; background: none; color: var(--muted); font-size: 12px; padding: 0 4px; text-decoration: underline; }
  .link:hover { color: var(--text); } .link.bad:hover { color: var(--bad); }
</style>
