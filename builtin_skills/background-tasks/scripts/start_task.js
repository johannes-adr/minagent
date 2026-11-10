// Usage: agent --js skills/background-tasks/scripts/start_task.js [name] <<'EOF' ... EOF
// Starts the shell command from stdin in the background and returns at once. Output goes
// to <log>, the task is recorded in tasks.json; both live in a per-working-directory
// folder in temp (see common.js). Old finished tasks are pruned first.
import fs from "node:fs";
import { spawn } from "node:child_process";
import { DIR, load, save, prune } from "./common.js";

const command = fs.readFileSync("/dev/stdin", "utf8").trim();
if (!command) {
  console.error("empty command: pipe the shell command into stdin");
  process.exit(2);
}

fs.mkdirSync(DIR, { recursive: true });
const tasks = await prune(load());
const id = String(Math.max(0, ...tasks.map((t) => Number(t.id))) + 1);
const log = `${DIR}/${id}.log`;
const exit = `${DIR}/${id}.exit`;

// `set -m` gives the job its own process group (pgid = pid), so stopping it also stops
// its children. The job writes its exit code to `exit` when it finishes. All of its stdio
// is redirected, so this wrapper (and the calling `execute`) returns immediately.
const wrapper = 'set -m; (sh -c "$1"; echo $? > "$3") > "$2" 2>&1 < /dev/null & echo $!';
const child = spawn("sh", ["-c", wrapper, "sh", command, log, exit], {
  stdio: ["ignore", "pipe", "inherit"],
});
let out = "";
child.stdout.on("data", (chunk) => (out += chunk));
child.on("close", (code) => {
  const pid = Number(out.trim());
  if (code !== 0 || !pid) {
    console.error(`cannot start the task (exit ${code})`);
    process.exit(1);
  }
  const name = globalThis.ARGS?.[0] ?? command.split("\n")[0].slice(0, 60);
  tasks.push({ id, name, command, pid, log, exit, started: new Date().toISOString() });
  save(tasks);
  console.log(`started task ${id} (pid ${pid}), log: ${log}`);
});
