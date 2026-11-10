---
name: subagents
description: Explains how to delegate a self-contained subtask to a subagent (a fresh agent process started with `--prompt`) and get its answer back. Use for independent research, review or implementation subtasks, for parallel work, or to keep large intermediate output out of your own context.
license: MIT
---

# Subagents

A subagent is a second agent process with the same tools and skills but **no memory of
this conversation**. It runs one prompt, streams its answer to stdout and exits.

## Spawning one

Run this with the `execute` tool, sandboxed as usual (no `disable_sandbox`):

```sh
AGENT --js skills/subagents/scripts/spawn_subagent.js <<'EOF'
<the complete prompt>
EOF
```

`AGENT` is the agent executable from the system prompt. The prompt goes through stdin via
a heredoc with a **quoted** delimiter (`'EOF'`), so quotes, `$`, backticks and newlines
need no escaping and there is no length limit. Only a line consisting of exactly `EOF`
would end it early; pick another delimiter (e.g. `'PROMPT_END'`) if the prompt contains one.

The script prints the subagent's final answer on stdout and exits with its exit code.
If it fails, the last lines of its log (tool calls and their output) are printed on stderr.

## Writing the prompt

- Put everything the subagent needs into the prompt: the goal, relevant file paths,
  constraints, what is already known. It cannot see this conversation.
- Say exactly what to return and keep it short, e.g. "Answer with a list of the files
  that call `foo()` and one line per file on how it is used". Its answer lands in your context.
- Say whether it may change files. It works in the same directory as you.

## Parallel subagents

Write each prompt to a file, start them in the background and wait for all of them:

```sh
cat > /tmp/task1.md <<'EOF'
...
EOF
cat > /tmp/task2.md <<'EOF'
...
EOF
S=skills/subagents/scripts/spawn_subagent.js
AGENT --js $S /tmp/task1.md > /tmp/task1.out 2>&1 &
AGENT --js $S /tmp/task2.md > /tmp/task2.out 2>&1 &
wait
cat /tmp/task1.out /tmp/task2.out
```

Give parallel subagents disjoint files to change.

## Limits

- An `execute` call is killed after 120 seconds, so give each subagent a task it can
  finish within that time; split bigger tasks.
- Nobody can approve the subagent's tool calls: it only gets sandboxed commands
  (no internet, no unsandboxed commands). Calls that need approval are denied, and it is
  told so.
- Check its answer before relying on it, especially claims about changed files.

You can also call the CLI directly: `AGENT --prompt "<text>"` (shell quoting applies,
and the command line is limited in length) or `AGENT --prompt -` with the prompt on
stdin. The answer streams to stdout, notes and tool output go to stderr.
