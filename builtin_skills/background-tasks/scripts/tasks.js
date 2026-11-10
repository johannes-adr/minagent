// Usage: agent --js skills/background-tasks/scripts/tasks.js [list | log <id> [lines] | stop <id> | clean]
//   list (default)   all tasks with status and their last log line
//   log <id> [lines] the last lines of a task's log (default 50)
//   stop <id>        stops the task and its child processes (SIGTERM, then SIGKILL)
//   clean            forgets finished tasks and deletes their logs
import { DIR, load, save, read, tail, sh, alive, status, remove } from "./common.js";

function find(tasks, id) {
  const t = tasks.find((t) => t.id === id);
  if (!t) {
    console.error(`no task ${id}`);
    process.exit(2);
  }
  return t;
}

const [cmd = "list", id, arg] = globalThis.ARGS ?? [];
const tasks = load();

if (cmd === "list") {
  if (!tasks.length) console.log("no tasks");
  else console.log(`tasks in ${DIR}`);
  for (const t of tasks) {
    console.log(`${t.id}  ${await status(t)}  pid ${t.pid}  started ${t.started}  ${t.name}`);
    const last = tail(read(t.log), 1);
    if (last) console.log(`    ${last}`);
  }
} else if (cmd === "log") {
  const t = find(tasks, id);
  console.log(tail(read(t.log), Number(arg) || 50));
} else if (cmd === "stop") {
  const t = find(tasks, id);
  if (!(await alive(t))) {
    console.log(`task ${id} is not running (${await status(t)})`);
  } else {
    // Negative pid: the whole process group of the task.
    await sh('kill -TERM -- "-$1" 2>/dev/null', String(t.pid));
    for (let i = 0; i < 20 && (await alive(t)); i++) await new Promise((r) => setTimeout(r, 100));
    if (await alive(t)) await sh('kill -KILL -- "-$1" 2>/dev/null', String(t.pid));
    console.log(`task ${id}: ${await status(t)}`);
  }
} else if (cmd === "clean") {
  const keep = [];
  for (const t of tasks) {
    if (await alive(t)) keep.push(t);
    else remove(t);
  }
  save(keep);
  console.log(`removed ${tasks.length - keep.length} finished tasks`);
} else {
  console.error(`unknown command ${cmd}; use list, log <id> [lines], stop <id> or clean`);
  process.exit(2);
}
