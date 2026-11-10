// Shared by start_task.js and tasks.js: where tasks are stored and how their state is checked.
import fs from "node:fs";
import os from "node:os";
import crypto from "node:crypto";
import { spawn } from "node:child_process";

// One directory per working directory, in temp: keeps the project clean and is writable
// in the sandbox.
const HASH = crypto.createHash("sha256").update(process.cwd()).digest("hex").slice(0, 12);
export const DIR = `${os.tmpdir().replace(/\/$/, "")}/agent-background-tasks/${HASH}`;
const FILE = `${DIR}/tasks.json`;

// Finished tasks are pruned when a new one starts: those older than MAX_AGE_MS, and all
// but the newest MAX_FINISHED.
const MAX_AGE_MS = 24 * 60 * 60 * 1000;
const MAX_FINISHED = 20;

export function load() {
  try {
    return JSON.parse(fs.readFileSync(FILE, "utf8"));
  } catch {
    return [];
  }
}

export function save(tasks) {
  fs.mkdirSync(DIR, { recursive: true });
  fs.writeFileSync(FILE, JSON.stringify(tasks, null, 2));
}

export function read(path) {
  try {
    return fs.readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

export function tail(text, n) {
  return (text ?? "").trimEnd().split("\n").slice(-n).join("\n");
}

// Runs `sh -c script args...` and resolves to its exit code.
export function sh(script, ...args) {
  return new Promise((resolve) => {
    const child = spawn("sh", ["-c", script, "sh", ...args], { stdio: "ignore" });
    child.on("close", (code) => resolve(code));
    child.on("error", () => resolve(1));
  });
}

// A pid alone may have been reused by an unrelated process, so the task counts as running
// only if the process with its pid still writes stdout to the task's log (Linux: /proc,
// macOS: lsof; `ps` is not allowed in the sandbox). Only then may `stop` signal it.
const OWNS_LOG = `out=$(readlink "/proc/$1/fd/1" 2>/dev/null || lsof -a -p "$1" -d 1 -Fn 2>/dev/null | sed -n 's/^n//p')
case "$out" in *"$2") exit 0 ;; esac
exit 1`;

export const alive = async (t) =>
  (await sh(OWNS_LOG, String(t.pid), `/${HASH}/${t.id}.log`)) === 0;

export async function status(t) {
  if (await alive(t)) return "running";
  const code = read(t.exit);
  return code === null ? "stopped" : `exited ${code.trim()}`;
}

export function remove(t) {
  for (const path of [t.log, t.exit]) fs.rmSync(path, { force: true });
}

// Returns the tasks to keep and deletes the files of the others.
export async function prune(tasks) {
  const running = [];
  const finished = [];
  for (const t of tasks) ((await alive(t)) ? running : finished).push(t);
  const now = Date.now();
  const keep = finished
    .filter((t) => now - Date.parse(t.started) < MAX_AGE_MS)
    .slice(-MAX_FINISHED);
  for (const t of finished) if (!keep.includes(t)) remove(t);
  return tasks.filter((t) => running.includes(t) || keep.includes(t));
}
