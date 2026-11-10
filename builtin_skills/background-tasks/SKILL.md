---
name: background-tasks
description: Explains how to run long-running shell commands (dev servers, watchers, long builds or test runs) in the background, then check their status and logs or stop them. Use when a command would exceed the 120 second `execute` limit or must keep running while you continue working.
license: MIT
---

# Background tasks

An `execute` call is killed after 120 seconds and waits for the command to finish. For
anything longer, or that must keep running (servers, watchers), start it as a background
task: it returns at once, keeps running, and is tracked per working directory in a temp
folder (`list` shows where) with its output in `<id>.log` there.

## Starting a task

```sh
AGENT --js skills/background-tasks/scripts/start_task.js dev-server <<'EOF'
npm run dev
EOF
```

`AGENT` is the agent executable from the system prompt. The command goes through stdin
(quoted heredoc, no escaping needed) and runs with `sh -c`. The optional argument is a
short name for the task. It prints the task id, pid and log file.

## Checking, reading logs, stopping

```sh
T=skills/background-tasks/scripts/tasks.js
AGENT --js $T              # list: id, status (running / exited <code> / stopped), last log line
AGENT --js $T log 1 100    # last 100 lines of task 1's log (default 50)
AGENT --js $T stop 1       # stop task 1 and all its child processes
AGENT --js $T clean        # forget finished tasks and delete their logs
```

## Notes

- Poll with `list` or `log` instead of `sleep`-ing for long; e.g. wait until the log
  shows a server is ready before using it.
- Stop tasks you no longer need, at the latest before you finish your work.
- Tasks run with the same sandbox as the `execute` call that started them.
- Finished tasks are kept for a day (at most the last 20), then pruned automatically
  when a new task starts.
