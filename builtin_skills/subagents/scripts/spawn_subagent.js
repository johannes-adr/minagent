// Usage: agent --js skills/subagents/scripts/spawn_subagent.js [prompt-file] [<<'EOF' ... EOF]
// Runs `agent --prompt -` with the prompt from the file, or from stdin without a file.
// The subagent's answer is streamed to stdout line by line; its progress (tool calls and
// their output) is printed to stderr only if it fails. Exits with the subagent's exit code.
import fs from "node:fs";
import { spawn } from "node:child_process";

const file = globalThis.ARGS?.[0];
const prompt = fs.readFileSync(file ?? "/dev/stdin", "utf8");
if (!prompt.trim()) {
  console.error("empty prompt: pass a prompt file or pipe the prompt into stdin");
  process.exit(2);
}

// Calls `emit` for every complete line of a stream of Buffers; `flush` emits the rest.
function lines(emit) {
  const decoder = new TextDecoder();
  let rest = "";
  return {
    push(chunk) {
      const parts = (rest + decoder.decode(chunk, { stream: true })).split("\n");
      rest = parts.pop();
      parts.forEach(emit);
    },
    flush() {
      rest += decoder.decode();
      if (rest) emit(rest);
      rest = "";
    },
  };
}

const LOG_TAIL = 50;
const log = [];
const out = lines((line) => console.log(line));
const err = lines((line) => {
  log.push(line);
  if (log.length > LOG_TAIL) log.shift();
});

// process.argv[0] is the agent executable this script runs in.
const child = spawn(process.argv[0], ["--prompt", "-"], { stdio: ["pipe", "pipe", "pipe"] });
child.stdout.on("data", (chunk) => out.push(chunk));
child.stderr.on("data", (chunk) => err.push(chunk));
child.on("error", (e) => {
  console.error(`cannot start the subagent: ${e}`);
  process.exit(1);
});
child.on("close", (code) => {
  out.flush();
  err.flush();
  if (code !== 0) {
    console.error(`subagent failed (exit ${code}); last ${log.length} log lines:\n${log.join("\n")}`);
  }
  process.exit(code ?? 1);
});
child.stdin.write(prompt);
child.stdin.end();
