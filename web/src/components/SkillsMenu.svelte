<script>
  import { chat, setSkills } from "../lib/chat.svelte.js";

  let open = $state(false);
  const skills = $derived(chat.skills);
  const names = $derived(skills?.available.map((s) => s.name) ?? []);
  const on = $derived(names.filter((n) => !skills.disabled.includes(n)));

  function toggle(name, enabled) {
    // What is switched off is stored, so skills added later are on in every chat.
    setSkills(names.filter((n) => (n === name ? !enabled : skills.disabled.includes(n))));
  }
</script>

{#if skills && names.length}
  <div class="menu">
    <button class="btn trigger" class:custom={skills.custom} title="Skills of this chat" onclick={() => (open = !open)}>
      Skills {on.length}/{names.length}
    </button>
    {#if open}
      <div class="scrim" onclick={() => (open = false)} role="presentation"></div>
      <div class="popover">
        <div class="title">Skills for this chat</div>
        {#each skills.available as skill (skill.name)}
          <label title={skill.description}>
            <input
              type="checkbox" checked={!skills.disabled.includes(skill.name)} disabled={chat.busy}
              onchange={(e) => toggle(skill.name, e.target.checked)}
            />
            <code>{skill.name}</code>
          </label>
        {/each}
        <div class="foot">
          {#if skills.custom}
            <span>differs from the default</span>
            <button class="btn" disabled={chat.busy} onclick={() => setSkills(null)}>Reset</button>
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
  .popover { position: absolute; right: 0; top: calc(100% + 6px); z-index: 5; min-width: 230px; max-width: 320px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; background: var(--surface); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 6px 24px rgba(0,0,0,.25); }
  .title { font-weight: 600; font-size: 13px; }
  label { display: flex; align-items: center; gap: 8px; font-size: 13px; cursor: pointer; }
  .foot { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-top: 4px; padding-top: 8px; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px; }
  .foot .btn { padding: 2px 10px; font-size: 12px; }
</style>
