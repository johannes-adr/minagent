<script>
  import Markdown from "./Markdown.svelte";
  import Reasoning from "./Reasoning.svelte";
  import ToolCard from "./ToolCard.svelte";
  import Question from "./Question.svelte";

  let { pane } = $props();
</script>

{#each pane.items as item (item.id)}
  {#if item.kind === "user"}
    <div class="user">{item.text}</div>
  {:else if item.kind === "assistant"}
    <Markdown source={item.raw} />
  {:else if item.kind === "reasoning"}
    <Reasoning {item} />
  {:else if item.kind === "tool"}
    <ToolCard tool={item} />
  {:else if item.kind === "question"}
    <Question {item} />
  {:else if item.kind === "note"}
    <div class="note">{item.text}</div>
  {:else if item.kind === "error"}
    <div class="error">{item.text}</div>
  {/if}
{/each}

<style>
  .user { align-self: flex-end; max-width: 85%; background: var(--user); padding: 8px 14px; border-radius: 14px 14px 4px 14px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .note { color: var(--muted); font-size: 13px; font-style: italic; }
  .error { border: 1px solid var(--bad); color: var(--bad); border-radius: 8px; padding: 8px 12px; white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
