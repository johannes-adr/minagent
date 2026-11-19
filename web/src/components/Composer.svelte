<script>
  import { cancel, chat, sendPrompt } from "../lib/chat.svelte.js";

  let text = $state("");
  let input;

  const fit = () => {
    input.style.height = "auto";
    input.style.height = input.scrollHeight + "px";
  };

  async function send() {
    const message = text.trim();
    if (!message || chat.busy) return;
    if (await sendPrompt(message)) {
      text = "";
      queueMicrotask(fit);
    }
  }
</script>

<footer>
  <div class="composer">
    <!-- svelte-ignore a11y_autofocus -->
    <textarea
      bind:this={input} bind:value={text} oninput={fit} rows="1" placeholder="Message the agent…" autofocus
      onkeydown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); } }}
    ></textarea>
    {#if chat.busy}
      <button class="stop" onclick={cancel}>Stop</button>
    {:else}
      <button class="send" disabled={!text.trim()} onclick={send}>Send</button>
    {/if}
  </div>
  <div class="hint">Enter to send · Shift+Enter for a new line</div>
</footer>

<style>
  footer { border-top: 1px solid var(--border); padding: 12px 20px 16px; }
  .composer { max-width: 800px; margin: 0 auto; display: flex; gap: 10px; align-items: flex-end; border: 1px solid var(--border); border-radius: 14px; background: var(--surface); padding: 8px 8px 8px 14px; }
  .composer:focus-within { border-color: var(--accent); }
  textarea { flex: 1; resize: none; border: 0; outline: 0; background: transparent; color: var(--text); font: inherit; max-height: 200px; padding: 4px 0; }
  .send, .stop { border-radius: 10px; padding: 6px 16px; border: 0; }
  .send { background: var(--accent); color: var(--accent-text); }
  .send:disabled { opacity: .4; cursor: default; }
  .stop { background: var(--bad); color: var(--bg); }
  .hint { max-width: 800px; margin: 6px auto 0; color: var(--muted); font-size: 12px; text-align: center; }
</style>
