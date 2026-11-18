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
| `fetch(url, { method, headers, body, secret })` | HTTP to the hosts declared in the call's `hosts` argument only |
| `fs.readText/writeText/list/exists(path)` | Files inside `work/` (a subdirectory of the harness's directory) only, never `.env*` |
| `runScript(path, args)` | Runs a script saved in `work/` in-process (an async function body that sees `args`); no process is started |

There is no `require`, no `import`, no timers, and no access to processes or environment
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

## Files

| File | Role |
|---|---|
| `mod.rs` | The tool (args `code`, `hosts`, `timeout_seconds`), its approval rule (`ToolPolicy`), the settings from config, and the tests |
| `runtime.rs` | Builds a fresh QuickJS runtime per call: memory limit, timeout, console capture; runs the script |
| `prelude.js` | Builds the script-facing `console`, `fetch` and `fs` from the native functions; the native functions themselves stay unreachable |
| `fetch.rs` | Checks scheme and host, injects secrets, sends the request with `reqwest` |
| `fs.rs` | Resolves paths and rejects anything outside `work/` or named `.env*` |

## How a call runs

1. **Approval** (`mod.rs`): if every host in `hosts` is listed in `AGENT_JS_ALLOWED_HOSTS`
   (or `hosts` is empty), the call runs without asking. Otherwise the user approves it, and
   sees the hosts and the full code.
2. **Run** (`runtime.rs`): the runtime is synchronous and runs on a blocking thread. `fetch`
   blocks until the response is complete, then returns an already-resolved promise. Without
   timers there is no event loop. When no jobs are left, the script is either done or waiting
   on a promise that can never settle; that is reported as an error instead of hanging.
3. **Output**: console output and any uncaught exception are formatted like the `execute`
   tool's output, so the model reads both the same way.

## Security rules

- **Network:**
  - Only `https://` is allowed, plus `http://` for `localhost` and `127.0.0.1`.
  - The host must be one of the call's declared `hosts`.
  - Redirects are not followed.
  - Responses are limited to 10 MB.
- **Secrets:**
  - Configured in `.env` as `AGENT_SECRET_<NAME>=<host> <header value>`.
  - `fetch(url, { secret: "name" })` makes the Rust side set the `Authorization` header,
    replacing any the script set itself. The script never sees the value.
  - A secret is only sent to its bound host.
- **Files:**
  - Absolute paths, `..`, and symlinks that lead outside `work/` are rejected.
  - So is any path component starting with `.env`.
  - `list` hides `.env*` entries.
- **Limits:**
  - 256 MB heap.
  - Timeout of 30 s by default, at most 300 s. It is enforced by an interrupt handler, so it
    also stops `while (true) {}`.
  - Each `fetch` gets only the time that remains.
  - Console output is capped at 1 MiB per stream.

## Configuration (`.env`)

```sh
AGENT_JS_ALLOWED_HOSTS=api.github.com,example.com   # calls to these hosts run without asking
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
