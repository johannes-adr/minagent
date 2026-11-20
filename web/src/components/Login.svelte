<script>
  import { auth, submitPassword } from "../lib/auth.svelte.js";

  const setup = $derived(auth.phase === "setup");
  let password = $state("");
  let repeat = $state("");
  let error = $state("");
  let working = $state(false);

  async function submit(e) {
    e.preventDefault();
    error = "";
    if (setup && password !== repeat) {
      error = "The passwords differ.";
      return;
    }
    working = true;
    try {
      await submitPassword(password, setup);
    } catch (err) {
      error = err.message;
    }
    working = false;
  }
</script>

<div class="wrap">
  <form onsubmit={submit}>
    <h1>agent</h1>
    <p>
      {#if setup}
        No password is set yet. Choose one; it is the only login.
      {:else}
        Sign in to continue.
      {/if}
    </p>
    <!-- svelte-ignore a11y_autofocus -->
    <input
      type="password" placeholder="Password" bind:value={password} autofocus
      autocomplete={setup ? "new-password" : "current-password"}
    />
    {#if setup}
      <input type="password" placeholder="Repeat password" bind:value={repeat} autocomplete="new-password" />
    {/if}
    {#if error}<div class="error">{error}</div>{/if}
    <button class="btn primary" disabled={working || !password}>{setup ? "Set password" : "Sign in"}</button>
  </form>
</div>

<style>
  .wrap { flex: 1; display: flex; align-items: center; justify-content: center; padding: 20px; }
  form { width: 100%; max-width: 340px; display: flex; flex-direction: column; gap: 10px; }
  h1 { margin: 0; font-size: 18px; font-weight: 600; }
  p { margin: 0 0 4px; color: var(--muted); font-size: 14px; }
  input { padding: 8px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font: inherit; }
  input:focus { outline: 0; border-color: var(--accent); }
  .btn { padding: 8px 14px; }
  .error { border: 1px solid var(--bad); color: var(--bad); border-radius: 8px; padding: 8px 12px; font-size: 14px; }
</style>
