# `execute_javascript`

Runs a JavaScript script written by the model inside the agent process, using
[QuickJS](https://bellard.org/quickjs/) (via `rquickjs`). It is meant for workflow automation:
calling APIs and transforming data.

The tool is built by default (Cargo feature `javascript`). To opt out:

```sh
cargo build --no-default-features
```

## Idea: the sandbox is the API

There is no container and no OS sandbox. Scripts are safe because they can only use what we
give them. A fresh runtime per call has exactly these globals:

| Global | What it can do |
|---|---|
| `console.log/info/warn/error` | Output; `log`/`info` go to stdout, `warn`/`error` to stderr |
| `fetch(url, { method, headers, body, secret, form, saveTo })` | HTTP to the hosts declared in the call's `hosts` argument only; requests that change data throw unless the call has `writes: true`; `form` sends multipart/form-data (files from `work/`), `saveTo` writes a successful response body to a file in `work/` (binary-safe) |
| `fs.readText/writeText/list/exists(path)` | Files inside `work/` (a subdirectory of the harness's directory) only, never `.env*`. `readText`/`list`/`exists` also accept `skill:<name>/<path>` (files of a skill, read-only) |
| `import ... from "skill:<name>/<path>.js"` | ES modules shipped by skills, read-only (see below) |
| `runScript(path, args)` | Runs a script saved in `work/` in-process (an async function body that sees `args`); no process is started |
| `agent(prompt, { description })` | Only where the caller has the `agent` tool: starts a subagent that sees only `prompt` and resolves to its final report (see below) |

There is no `require`, no other `import`, no timers, and no access to processes or environment
variables. Top-level `await` works because the script runs as an ES module.

```js
const r = await fetch("https://api.github.com/repos/rust-lang/rust", { secret: "GITHUB" });
const repo = await r.json();
fs.writeText("out/stars.txt", String(repo.stargazers_count));
console.log(repo.full_name, repo.stargazers_count);
```

A script cannot start a process, but it can save another script and run it in-process, with
arguments, with `runScript(path, args)`:

```js
fs.writeText("greet.js", "console.log('hello', args.join(' ')); return args.length;");
await runScript("greet.js", ["a", "b"]); // prints "hello a b", resolves to 2
```

## Subagents from scripts: `agent()`

A script can loop over many items and hand each one to a subagent, so the loop is code and
only what the script prints reaches the caller's context:

```js
for (const id of [12, 15, 31]) {
  const report = await agent(`Propose fields for Paperless document #${id} ... one line of JSON`, { description: `#${id}` });
  console.log(report);
}
```

- **Who has it:** scripts of an agent that has the `agent` tool (the main agent, or a webhook
  run that lists `agent`). A subagent's scripts don't, so subagents can't nest.
- **Read-only:** the subagent has the caller's tools except `agent`, `ask_user` and `execute`
  (`Toolset::for_script_agents`), and its own `execute_javascript` calls with `writes: true`
  are refused without asking. All writes stay in the caller's one approved call.
- **Limits:** at most 20 subagents per call, one after another (the runtime blocks in host
  calls). Time inside `agent()` doesn't count against `timeout_seconds`; each call has its own
  limit of 30 minutes.
- **Stop:** dropping the tool call (stop in the UI, or a webhook run's time limit) closes a
  channel that the interrupt handler and a running `agent()` watch, so the script and its
  subagent end too.
- The report is untrusted text: parse it with `try`.

## Skill modules

Skills (`skills/<name>/`) may ship JavaScript ES modules that scripts import:

```js
import { listIssues } from "skill:github/scripts/github.js";
const template = fs.readText("skill:github/assets/template.md");
```

- Only `skill:<name>/<path>` specifiers work. Bare names, URLs, absolute paths and relative
  imports from the main script fail. Inside a skill module, `./x.js` and `../lib/y.js` resolve
  relative to the module and must stay in the same skill; `skill:<other>/...` may import
  another skill.
- Names are normalized to `skill:<name>/<path>`, so a module is evaluated once however it is
  imported.
- Skills are **read-only** from scripts (`fs.writeText("skill:...")` fails). The script runs
  without approval and can write `work/`, so anything it writes is untrusted, while a skill's
  `SKILL.md` goes into the system prompt. That is why skills do not live in `work/`.
- Skills cannot declare hosts or secrets; approval depends only on the call's `hosts`.
- Files are read with the same confinement as `fs` (`fs::resolve`, applied to the skills root,
  `modules::skills_root()`): no `..`, no symlinks out of the root, no `.env*`.

## Files

| File | Role |
|---|---|
| `mod.rs` | The tool (args `code`, `hosts`, `timeout_seconds`, `writes`), its approval rule (`ToolPolicy`), the settings from config, and the tests |
| `runtime.rs` | Builds a fresh QuickJS runtime per call: memory limit, timeout, console capture; runs the script |
| `prelude.js` | Builds the script-facing `console`, `fetch` and `fs` from the native functions; the native functions themselves stay unreachable |
| `fetch.rs` | Checks scheme and host, injects secrets, sends the request with `reqwest` |
| `fs.rs` | Resolves paths and rejects anything outside `work/` (or the skills root for `skill:` paths) or named `.env*` |
| `modules.rs` | Resolver and loader for `skill:` ES modules; the skills root |

## How a call runs

1. **Approval** (`mod.rs`): if every host in `hosts` is listed in `AGENT_JS_ALLOWED_HOSTS`
   (or `hosts` is empty) and the call is read-only, it runs without asking. A call is
   read-only unless it says `writes: true`: `fetch` then throws for every method but
   GET/HEAD/OPTIONS. A call with `writes: true` needs approval as a whole (one approval, also
   for hosts that are not allowed), unless `AGENT_JS_ALLOW_WRITES=1` and all hosts are
   allowed. The user sees the hosts and the full code. A webhook run has its own rules
   (`frontend/hook_run.rs`): its own hosts and writes mode replace the configured ones
   (`instance_approval`), it can park a writing call until the user decides, and then run
   it with `run_approved`. Every request that changed data is listed at the end of the
   call's output (`requests that changed data:`), so the transcript shows what it did.
2. **Run** (`runtime.rs`): the runtime is synchronous and runs on a blocking thread. `fetch`
   blocks until the response is complete, then returns an already-resolved promise. Without
   timers there is no event loop. When no jobs are left, the script is either done or waiting
   on a promise that can never settle; that is reported as an error instead of hanging.
3. **Output**: console output and any uncaught exception are formatted like the `execute`
   tool's output, so the model reads both the same way.

## Security rules

- **Network:**
  - Only `https://` and `http://` are allowed. Over `http://`, requests and secrets go
    unencrypted.
  - The host must be one of the call's declared `hosts`.
  - Redirects are not followed.
  - Responses are limited to 10 MB; responses saved with `saveTo` and files sent in a `form`
    to 100 MB.
  - `form` files and `saveTo` targets go through the same path confinement as `fs` (`saveTo`
    never writes to `skill:` paths). An error response (not 2xx) is returned as text instead of
    saved.
- **Secrets:**
  - Configured in `.env` as `AGENT_SECRET_<NAME>=<host> <header value>`.
  - `fetch(url, { secret: "name" })` makes the Rust side set the `Authorization` header,
    replacing any the script set itself. The script never sees the value.
  - A secret is only sent to its bound host.
- **Files:**
  - Absolute paths, `..`, and symlinks that lead outside `work/` are rejected.
  - `skill:` paths and imports are read-only and confined to the skills directory the same way.
  - So is any path component starting with `.env`.
  - `list` hides `.env*` entries.
- **Limits:**
  - 256 MB heap.
  - Timeout of 30 s by default, at most 300 s. It is enforced by an interrupt handler, so it
    also stops `while (true) {}`. Time spent waiting in `agent()` is not counted.
  - Each `fetch` gets only the time that remains.
  - Console output is capped at 1 MiB per stream.

## Configuration (`.env`)

```sh
AGENT_JS_ALLOWED_HOSTS=api.github.com,example.com   # calls to these hosts run without asking
AGENT_JS_ALLOW_WRITES=1                             # ...and calls with writes: true (POST/PUT/PATCH/DELETE) there run without asking too
AGENT_SECRET_GITHUB=api.github.com Bearer ghp_xxx   # used as fetch(url, { secret: "GITHUB" })
```

## Adding a host function

1. Write the native function in Rust: a new file like `fs.rs`, or next to the existing ones.
   Put every check there, not in JavaScript.
2. Register it on the `native` object in `runtime::install`.
3. Expose it to scripts in `prelude.js`, converting arguments to plain strings or numbers.
4. Document it in the tool's prompt (`ToolPolicy::prompt` in `mod.rs`) and in the table above,
   and add a test.

## Known limitations

- **Echoed secrets:** a server that echoes request headers back in its response would let the
  script read a secret.
- **Private addresses:** hosts are checked by name only, so a hostname that resolves to a
  private IP is not blocked.
- **Sequential requests:** `Promise.all` over several `fetch` calls runs them one after
  another, because `fetch` blocks.
- **Large files:** the heap limit only covers JavaScript. A huge `fs.readText` is read into
  Rust memory first.
