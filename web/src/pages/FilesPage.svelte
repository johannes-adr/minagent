<script>
  import { onMount } from "svelte";
  import { go } from "../lib/route.svelte.js";
  import * as fs from "../lib/files.js";

  let root = $state("skills");
  let dir = $state(""); // directory shown in the list
  let entries = $state([]);
  let error = $state("");

  let file = $state(null); // {path, readonly, notText}
  let text = $state("");
  let original = $state("");
  let saving = $state(false);
  let saved = $state(false);

  const dirty = $derived(file && !file.notText && !file.readonly && text !== original);
  const crumbs = $derived(dir ? dir.split("/") : []);
  const rootInfo = $derived(fs.ROOTS.find((r) => r.id === root));

  async function show(path = dir) {
    error = "";
    try {
      entries = await fs.list(root, path);
      dir = path;
    } catch (e) {
      error = e.message;
    }
  }

  // Asks before leaving unsaved changes behind.
  const discard = () => !dirty || confirm("Discard unsaved changes?");

  async function chooseRoot(id) {
    if (id === root || !discard()) return;
    root = id; file = null; await show("");
  }

  async function open(path) {
    if (!discard()) return;
    error = ""; saved = false;
    try {
      const data = await fs.read(root, path);
      file = { path, readonly: data.readonly, notText: false };
      text = original = data.content;
    } catch (e) {
      file = { path, readonly: true, notText: true, reason: e.message };
      text = original = "";
    }
  }

  async function activate(entry) {
    const path = fs.join(dir, entry.name);
    if (entry.dir) { if (discard()) await show(path); } else await open(path);
  }

  async function save() {
    if (!dirty || saving) return;
    saving = true; error = "";
    try {
      await fs.write(root, file.path, text);
      original = text; saved = true;
      await show();
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  // Runs a change, then shows the directory again.
  async function change(action) {
    error = "";
    try { await action(); await show(); } catch (e) { error = e.message; }
  }

  function newFile() {
    const name = prompt("Name of the new file");
    if (!name?.trim() || !discard()) return;
    const path = fs.join(dir, name.trim());
    change(async () => { await fs.write(root, path, ""); await open(path); });
  }
  function newFolder() {
    const name = prompt("Name of the new folder");
    if (name?.trim()) change(() => fs.mkdir(root, fs.join(dir, name.trim())));
  }
  function newSkill() {
    const name = prompt("Name of the new skill (lowercase letters, digits and hyphens)");
    if (!name?.trim() || !discard()) return;
    const id = name.trim();
    if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(id)) { error = "A skill name has lowercase letters, digits and single hyphens only."; return; }
    const path = `${id}/SKILL.md`;
    change(async () => { await fs.write(root, path, fs.skillTemplate(id)); await open(path); });
  }
  function renameEntry(entry) {
    const from = fs.join(dir, entry.name);
    const to = prompt("New path", from);
    if (!to?.trim() || to.trim() === from) return;
    change(async () => {
      await fs.rename(root, from, to.trim());
      if (file && (file.path === from || file.path.startsWith(from + "/"))) file = null;
    });
  }
  function removeEntry(entry) {
    const path = fs.join(dir, entry.name);
    if (!confirm(`Delete ${entry.dir ? "the folder " : ""}${path}${entry.dir ? " and everything in it" : ""}?`)) return;
    change(async () => {
      await fs.remove(root, path);
      if (file && (file.path === path || file.path.startsWith(path + "/"))) file = null;
    });
  }

  function onkeydown(e) {
    if ((e.metaKey || e.ctrlKey) && e.key === "s") { e.preventDefault(); save(); }
  }

  const size = (n) => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / 1048576).toFixed(1)} MiB`);

  onMount(() => show(""));
</script>

<svelte:window {onkeydown} onbeforeunload={(e) => { if (dirty) e.preventDefault(); }} />

<header>
  <button class="btn" onclick={() => discard() && go("/")}>← Chat</button>
  <h1>Files</h1>
  <div class="roots">
    {#each fs.ROOTS as r}
      <button class="root" class:active={r.id === root} onclick={() => chooseRoot(r.id)}>{r.label}</button>
    {/each}
  </div>
  <span class="hint">{rootInfo.hint}</span>
</header>

<div class="body">
  <aside>
    <div class="bar">
      <nav class="crumbs">
        <button onclick={() => discard() && show("")}>{root}</button>
        {#each crumbs as name, i}
          <span>/</span><button onclick={() => discard() && show(crumbs.slice(0, i + 1).join("/"))}>{name}</button>
        {/each}
      </nav>
      <div class="actions">
        {#if root === "skills" && !dir}<button class="btn" onclick={newSkill}>+ Skill</button>{/if}
        <button class="btn" onclick={newFile}>+ File</button>
        <button class="btn" onclick={newFolder}>+ Folder</button>
      </div>
    </div>
    {#if error}<div class="error">{error}</div>{/if}
    <ul>
      {#if dir}
        <li><button class="row" onclick={() => discard() && show(fs.parent(dir))}>📁 ..</button></li>
      {/if}
      {#each entries as entry (entry.name)}
        <li class:current={file?.path === fs.join(dir, entry.name)}>
          <button class="row" onclick={() => activate(entry)}>
            <span class="icon">{entry.dir ? "📁" : "📄"}</span>
            <span class="name">{entry.name}</span>
            {#if entry.readonly}<span class="lock" title="Built in: replaced on every start">🔒</span>{/if}
            {#if !entry.dir}<span class="size">{size(entry.size)}</span>{/if}
          </button>
          <span class="tools">
            <button title="Rename or move" onclick={() => renameEntry(entry)}>✎</button>
            <button title="Delete" onclick={() => removeEntry(entry)}>✕</button>
          </span>
        </li>
      {:else}
        <li class="empty">Empty.</li>
      {/each}
    </ul>
  </aside>

  <section>
    {#if !file}
      <div class="placeholder">Select a file to view or edit it.</div>
    {:else}
      <div class="bar">
        <code class="path">{root}/{file.path}</code>
        {#if dirty}<span class="dirty">unsaved</span>{:else if saved}<span class="ok">saved</span>{/if}
        <span class="spacer"></span>
        <button class="btn primary" disabled={!dirty || saving} onclick={save}>{saving ? "Saving…" : "Save"}</button>
      </div>
      {#if file.readonly && !file.notText}
        <div class="note">Built-in skill file: it is replaced on every start, so it can't be changed here. Copy the skill under another name to adapt it.</div>
      {/if}
      {#if file.notText}
        <div class="placeholder">{file.reason}</div>
      {:else}
        <textarea bind:value={text} readonly={file.readonly} spellcheck="false" wrap="off"></textarea>
      {/if}
    {/if}
  </section>
</div>

<style>
  header { display: flex; align-items: center; gap: 12px; padding: 12px 20px; border-bottom: 1px solid var(--border); }
  h1 { margin: 0; font-size: 15px; font-weight: 600; }
  header .btn { white-space: nowrap; }
  .roots { display: flex; gap: 4px; }
  .root { padding: 4px 12px; border-radius: 8px; border: 1px solid transparent; background: none; color: var(--muted); }
  .root.active { border-color: var(--border); background: var(--surface); color: var(--text); }
  .hint { color: var(--muted); font-size: 12px; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .body { flex: 1; min-height: 0; display: flex; }
  aside { width: min(380px, 42vw); display: flex; flex-direction: column; border-right: 1px solid var(--border); background: var(--surface); min-height: 0; }
  section { flex: 1; min-width: 0; display: flex; flex-direction: column; min-height: 0; }
  .bar { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--border); flex-wrap: wrap; }
  .actions { display: flex; gap: 6px; margin-left: auto; }
  .actions .btn { padding: 2px 10px; font-size: 12px; }
  .crumbs { display: flex; align-items: center; gap: 4px; font: 13px var(--mono); min-width: 0; flex-wrap: wrap; }
  .crumbs button { border: 0; background: none; color: var(--accent); padding: 0; font: inherit; }
  .crumbs span { color: var(--muted); }
  ul { list-style: none; margin: 0; padding: 4px; overflow-y: auto; flex: 1; }
  li { display: flex; align-items: center; border-radius: 8px; }
  li:hover, li.current { background: var(--code); }
  li.current { background: var(--user); }
  .row { flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; padding: 5px 8px; border: 0; background: none; color: var(--text); text-align: left; }
  .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .size, .lock { color: var(--muted); font-size: 11px; }
  .tools { display: none; gap: 2px; padding-right: 4px; }
  li:hover .tools { display: flex; }
  .tools button { border: 0; background: none; color: var(--muted); padding: 2px 6px; border-radius: 6px; }
  .tools button:hover { color: var(--text); }
  .empty { color: var(--muted); padding: 8px; font-size: 13px; }
  .error { margin: 8px 12px 0; border: 1px solid var(--bad); color: var(--bad); border-radius: 8px; padding: 6px 10px; font-size: 13px; overflow-wrap: anywhere; }
  .note { margin: 10px 12px 0; border: 1px solid var(--warn); color: var(--warn); border-radius: 8px; padding: 6px 10px; font-size: 13px; }
  .placeholder { margin: auto; color: var(--muted); padding: 20px; text-align: center; }
  .path { font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .spacer { flex: 1; }
  .dirty { color: var(--warn); font-size: 12px; } .ok { color: var(--ok); font-size: 12px; }
  textarea { flex: 1; min-height: 0; margin: 10px 12px 12px; padding: 10px 12px; resize: none; border-radius: 8px; border: 1px solid var(--border); background: var(--code); color: var(--text); font: 13px/1.55 var(--mono); tab-size: 2; }
  textarea:focus { outline: 0; border-color: var(--accent); }
  textarea[readonly] { opacity: .75; }
  @media (max-width: 800px) { .body { flex-direction: column; } aside { width: auto; height: 45%; border-right: 0; border-bottom: 1px solid var(--border); } .hint { display: none; } }
</style>
