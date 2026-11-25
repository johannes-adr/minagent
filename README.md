# agent

LLM agent harness with a terminal UI, a browser UI and an [ACP](https://agentclientprotocol.com) server.
Commands run inside a container; the workspace is the only host directory it sees.

## Install

Latest release (macOS arm64 or Linux x86_64) into the current directory:

```sh
p=$([ "$(uname)" = Darwin ] && echo macos-arm64 || echo linux-x86_64)
t=$(curl -fsSL https://api.github.com/repos/johannes-adr/minagent/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
curl -fsSL "https://github.com/johannes-adr/minagent/releases/download/$t/agent-$t-$p.tar.gz" | tar -xz
```

Or use [`install.sh`](install.sh), which also verifies the checksum.

## Run in a container

```sh
cp .env.example .env          # in your workspace; fill in LLM_BACKEND, API key, ...
/path/to/agent/agent.sh       # terminal UI
/path/to/agent/agent.sh --web # browser UI
/path/to/agent/agent.sh --acp # ACP over stdio
```

[`agent.sh`](agent.sh) rebuilds the image only when its sources changed, then runs it with the
current directory as workspace (podman if installed, else docker; override with
`AGENT_RUNTIME=docker`). For an ACP client, use it as the agent command, e.g. Zed `settings.json`:

```json
"agent_servers": {
  "agent": { "command": "/path/to/agent/agent.sh", "args": ["--acp"] }
}
```

Without the script:

```sh
docker build -t agent .
docker run --rm -it -v "$PWD:$PWD" -w "$PWD" agent        # TUI (needs -t)
docker run --rm -i  -v "$PWD:$PWD" -w "$PWD" agent --acp  # ACP (no -t, it would garble the protocol)
```

Notes:

- The workspace is mounted under its host path, so the client's `cwd` exists in the container. The client must start the agent in the project directory.
- Config comes from `config.json` (written by the web UI's settings page), `.env` in the workspace or the environment (`-e LLM_BACKEND=... -e ANTHROPIC_API_KEY=...` without the script); see [Settings](#settings-web-ui).
- Ollama on the host: `OLLAMA_API_BASE_URL=http://host.docker.internal:11434` (podman: `host.containers.internal`).
- Podman: add `--userns=keep-id` so files keep your user (the script does). Docker on Linux: add `--user "$(id -u):$(id -g)"`.
- The container has full network access, and commands in it can read `.env` and `config.json`. Use keys with minimal scope.

## `AGENTS.md`

If the working directory contains an [`AGENTS.md`](https://agents.md), its contents are added to the system prompt (of the main agent and its subagents) on startup. Restart the agent after editing it.

## Browser UI

`agent.sh --web` prints a URL like `http://localhost:8080/#token=...`; open it. The page streams the answer as markdown, shows each tool call as a card with all of its arguments (e.g. the full script of an `execute_javascript` call) and its output (open while pending, running or failed, folded once done) and asks for approval with Allow/Deny buttons where the terminal UI would ask `y/N`. Markdown is rendered in full (tables, nested and task lists, code blocks with copy button, links limited to http(s)/mailto; images are shown as links so the model can't make the page load URLs). A subagent gets its own tab in a side panel where its answer and tool calls (with approvals) stream like the main agent's. The model's reasoning streams into a "Thinking" block that folds away once the answer starts. Stop cancels the running prompt. Reloading the page or opening a second tab shows the same session. With the Ollama backend a "Thinking" slider appears in the header if the model lists thinking levels in `/api/show` (e.g. `qwen3.8`: off, low, medium, xhigh; on/off models get a two-step switch); until you move it the model's default applies, and the choice is kept until the server restarts.
- Sessions are saved after every prompt as `sessions/<id>.json` (history plus transcript; git-ignored). The ☰ Sessions button lists them to open, delete or start a new one (not while the agent is working). The agent starts with a new session.

- The agent can run commands, so the API needs a login. There is one user and one password: the first time the page is opened it asks you to choose the password (any, just not empty), after that it asks for it. Only an Argon2id hash is stored, in `auth.json` next to `config.json` (mode 0600, git-ignored); delete the file to set a new password. Wrong passwords are answered after a second, one at a time; with more than 5 attempts at once the rest are refused (`429`). Whoever reaches the page first sets the password, so do that before exposing the port. Logins are kept in memory: a restart logs you out. The random token printed at startup (`#token=…` in the URL) is still accepted. The script publishes the port on `127.0.0.1` only; `AGENT_WEB_PORT=9000` changes it.
- Without the script: `agent --web [addr]` (default `127.0.0.1:8080`). In a container use `agent --web 0.0.0.0:8080` with `-p 127.0.0.1:8080:8080`.
- The page is a [Svelte](https://svelte.dev) app in [`web/`](web/) without external resources, so it works offline. Vite builds it to static files (`web/dist`) that are embedded in the binary and served by the agent; Node is only needed to build. `cargo build` works without it but then serves a placeholder page: run `npm install && npm run build` in `web/` first (`agent.sh` and the Dockerfile do this for you). For UI development run `npm run dev` in `web/` (hot reload, forwards `/api` to a running `agent --web`; `AGENT_URL` changes the target). Debug builds of the agent read `web/dist` from disk, so rebuilt UI files show up without recompiling.

### Settings (web UI)

The gear button opens a settings page with every option of `.env.example` (the list in `src/config.rs` drives both). Saving writes `config.json` in the workspace (`AGENT_CONFIG` names another file; mode 0600, git-ignored) and applies it to the next message without a restart: model, API keys, context window, tools and the `execute_javascript` hosts and secrets. The order of priority is `config.json`, then the environment, then `.env`; "Reset" removes a value from the file so the environment's applies again. Secrets (API keys, `AGENT_SECRET_*`) are never sent back to the page, which only shows whether they are set (and a secret's host); new secrets can be added under "Secrets". Without an API key the web UI still starts, so the key can be entered there. Settings can only be saved while the agent is not working.

The "Tools" menu in the chat header turns tools on and off for the current chat only (it overrides `AGENT_TOOLS` / the default from the settings page, is saved with the session, and "Reset" goes back to the default). Subagents get the same tools as their parent, except `agent`. Skills work the same way: the "Skills" menu switches skills off for the current chat (default: all on, including skills you add later; the default can be changed with `AGENT_DISABLED_SKILLS` on the settings page). A skill that is off is not listed in the system prompt, `use_skill` doesn't know it and scripts can't import or read it (`skill:<name>/…`). It is still a file, so with `execute` the agent can read it anyway: this keeps the prompt short and the agent focused, it is not a lock. The choice is saved with the session.

### Files (web UI)

The 🗂 button opens a file browser on two areas: `skills/` and the agent's working directory `work/` (nothing else, such as `.env` or the sources, is reachable; paths with `..` and symbolic links are refused). Text files up to 1 MiB open in an editor (Ctrl/Cmd+S saves); files and folders can be added, renamed, moved and deleted. "+ Skill" creates `skills/<name>/SKILL.md` from a template. The files of built-in skills are rewritten on every start, so they are read-only (a lock in the list); add your own files next to them or copy a skill under another name. After skills were changed, the agent is rebuilt before the next message so its system prompt lists them.


After each model call of the main agent the UIs show how full its context is, e.g. `42k / 200k tokens (21%)`: the terminal UI at the right of the status line (yellow from 80 %, red from 95 %), the browser UI in the header, and ACP clients get a `usage_update` session update (`used`, `size`). The tokens are the input and output of the latest call as the provider reports them, i.e. what the next request sends again. Subagents have their own context and are not counted.

The context window comes from rig's built-in model catalog (where the provider caps input below the window, that lower limit). Models it doesn't know, which includes most Ollama models, show only the tokens, and ACP clients get no update; set `AGENT_CONTEXT_WINDOW=32768` in `.env` to give the size. For Ollama, use the context length the daemon runs the model with (`num_ctx`, `OLLAMA_CONTEXT_LENGTH`), not the model's maximum.

### Webhooks

The ⚡ button manages webhooks: an outside service starts the agent with `POST /hooks/<name>`. A hook has its own token (shown once when it is created or renewed; only a hash is stored in `hooks.json`, next to `config.json`, mode 0600, git-ignored), a prompt template, the tools, skills and hosts its runs may use, an optional callback URL and a time limit. The token goes in `Authorization: Bearer <token>`, `X-Hook-Token` or `?token=`; it is valid only for that hook, and the page's token is not needed (and does not work there).

```sh
curl -X POST http://localhost:8080/hooks/github-issues \
  -H "Authorization: Bearer <token>" -H "Content-Type: application/json" \
  -d '{"issue": {"title": "Crash on start"}}'      # 202 {"session": "…", "queued": 1}
```

- **Built-in workflows:** the program ships hooks ready to use ([`builtin_workflows/`](builtin_workflows), embedded in the binary). The hooks page lists them under "Built in"; one click fills the form (prompt, tools, skills, write mode, time limit, and the host of the matching `AGENT_SECRET_<NAME>`), and saving it creates a normal hook with its own token that you can change afterwards. Nothing is created without you, and a hook you saved is not overwritten by later versions. `paperless-consume`: Paperless-ngx calls it when it has consumed a document; the agent sorts it (see the `paperless` skill) and writes after your approval.
- **Prompt template:** `{{payload}}` (the raw body, cut at 50,000 characters), `{{body.issue.title}}` (a field of a JSON body; arrays by index, e.g. `{{body.items.0.name}}`), `{{headers.x-github-event}}` (not `authorization`, `x-hook-token` or `cookie`), `{{hook}}`. Missing values are empty. Whatever the request contains is untrusted text that reaches the model.
- **Queue:** every call is a run in a FIFO queue and answers `202` at once with the id of its session; `429` once 50 runs wait. Runs go one after the other, after what you do in the chat (a running prompt is not interrupted). Each run is a session of its own, titled `hook: <name>`, in the sessions list; the chat you have open is not touched. A run that is still waiting when the agent restarts stays "queued" and is not run.
- **Tools and skills:** a run has only the tools listed for the hook (never `ask_user`) and no skills unless the hook lists them (the default is none, and skills added later stay off).
- **Nobody to ask:** everything that would ask for approval is declined (the model is told and can try another way). The exceptions are the hook's own allowances: scripts (`execute_javascript`) may reach the listed hosts, which replace `AGENT_JS_ALLOWED_HOSTS` for this run, and may change data there (a call with `writes: true`) as the hook's **writes** mode says: `deny` (the call is declined), `approve` (the default: the run pauses until you decide, see below) or `allow` (the call runs without asking). Calls that name other hosts are declined in every mode. In `hooks.json`, `writes` replaces the older `allow_writes` flag (`true` is read as `allow`, otherwise `approve`). The configured secrets stay usable, but only for the hosts they are bound to. `view_image` outside `work/` is declined. `execute` (a shell) can only be allowed for a hook when `AGENT_ISOLATED=1` (the container); elsewhere it is not available to hooks.
- **Result:** read it in the session, or set a callback URL: it gets a `POST` with `{"hook", "session", "status": "done"|"failed"|"timeout"|"waiting", "result"}` (the agent's last answer). The callback is not retried.
- **Approvals (mode `approve`):** when a run makes a `writes: true` call, the session is *parked*: the call (script, hosts, title) and the agent's last message are stored in the session file (`pending`), the run stops without the model seeing a refusal, and the worker is free for the next job, so a waiting run never blocks the chat. The header's "Approvals" button (with the number of waiting runs, updated live) opens the approvals page: one card per run with the agent's message, the script folded, a comment field, Approve and Decline, and a link to the session. Deciding queues a continuation job: the session is rebuilt from disk; if approved, the stored call runs (writes allowed for this one call, only on the hook's hosts) and the agent gets its output in a message and checks the result, fixing failures itself (another write needs another approval); if declined, the agent is told, with your comment. A continuation can park again. The time limit applies to each running segment, not to the waiting. The waiting state lives in the session file, so it survives a restart; a decision that was queued but not run yet when the agent restarts is lost, and the run is simply waiting again. Deciding is refused (`409`) for a session that is not waiting or whose hook was deleted or switched off, and `404` for an unknown one. Opening a waiting session in the chat and continuing it there ends the waiting.
  The API (token as for the page): `GET /api/approvals` lists the waiting runs (`session`, `hook`, `time`, `message`, `title`, `hosts`, `code`), `POST /api/approvals/decide` takes `{"session", "approve": true|false, "comment"?}` and answers `202`.

### Updates

The header shows the running version (`agent --version` prints it too). The agent asks GitHub for the newest release once an hour; when there is a newer one, an "Update to vX.Y.Z" button appears. It downloads the release archive for this system (macOS arm64 or Linux x86_64), checks its SHA-256 sum, unpacks it next to the running program, checks that the new program reports the right version, moves it over the old one and starts it in place of the running process: same arguments (`--web …`), same environment, same working directory. You have to log in again, as after any restart; sessions and waiting approvals are on disk and stay. The update is refused while webhook runs wait in the queue (a restart would drop them). The sum comes from the same release as the file, so it guards against a broken download, not against a hijacked release.

Not offered: in a container (`AGENT_ISOLATED=1`: pull the new image instead; the button is replaced by a note), on other systems, and for a build from source (version `0.1.0`).

## Skills

Skills ([Agent Skills](https://agentskills.io/specification) format) live in `skills/<name>/`. Only their names and descriptions go into the system prompt; the `use_skill` tool loads a skill's `SKILL.md` when a task needs it, and with `file` (e.g. `references/search.md`) a further document the instructions name, so long reference material is only read when needed.

Built-in skills are embedded in the binary from [`builtin_skills/`](builtin_skills) and reinstalled into `skills/` on every start (edit them in `builtin_skills/`):

- `creating-skills`: how to write a new skill.
- `paperless`: works with a [Paperless-ngx](https://docs.paperless-ngx.com) archive (search and answer from documents, inbox triage, custom fields, bulk edits, upload/download, reports, cleanup) through a JavaScript client for `execute_javascript`. Setup in `.env`: `AGENT_SECRET_PAPERLESS=<host> Token <api token>` (token from "My Profile" in the Paperless web UI; host without scheme and port) and, to skip approvals for reading, the host in `AGENT_JS_ALLOWED_HOSTS`; changes need approval of the whole script (`writes: true`) unless `AGENT_JS_ALLOW_WRITES=1`. `references/consume.md` is a workflow for a webhook that fires when Paperless has consumed a document: the agent proposes title, correspondent, type, tags, date and fields as a table and writes them in one call, which you approve on the approvals page (a `work/paperless-conventions.md` that you maintain holds your title scheme and tag rules). The agent asks for the URL once and saves it in `work/paperless.json` after a successful connection; it does not change it on its own. Written for Paperless-ngx 3.x (API version 10).

## `view_image` tool

`view_image(path)` shows a PNG, JPEG, GIF or WebP file (max 5 MiB) to the model, e.g. a screenshot or chart made with `execute`. The type is detected from the file's content, so other files (such as `.env`) are rejected. It is built by default (Cargo feature `image`; opt out with `cargo build --no-default-features --features javascript`). Relative paths are relative to the `work/` directory (a subdirectory of the harness's directory, shared with `execute_javascript`); images inside it (after resolving `..` and symlinks) are shown without asking, and in a container all are; elsewhere each call needs approval, as the file is sent to the model provider.

The model must accept images: Anthropic and OpenAI models do; with Ollama use a vision model (e.g. `LLM_MODEL=llama3.2-vision`), as the default `qwen3` is text-only. Ollama cannot carry images inside tool results, so rig attaches them to a following user message instead.

## `execute_javascript` tool

For workflow automation (scripts that call APIs and transform data) there is a tool that runs JavaScript in-process with [QuickJS (rquickjs)](https://crates.io/crates/rquickjs). It is built by default (Cargo feature `javascript`). To opt out:

```sh
cargo build --no-default-features
```

The script is an ES module (top-level `await` works) and gets only this API: no `require`, no npm, no timers, no process or environment access. `import` works only for the JavaScript modules of skills: `import { connect } from "skill:<name>/<path>.js"` (read-only; see [Skills](#skills)).

- `console.log/info/warn/error`: captured and returned, together with the error if the script throws.
- `fetch(url, {method, headers, body, secret, form, saveTo})`: resolves to `{status, ok, headers, text(), json()}`. A call is **read-only** by default: a request that changes data (any method but GET/HEAD/OPTIONS, including multipart uploads) throws "this call is read-only" inside the script. Only `https://` and `http://` URLs to hosts in the call's `hosts` argument work (with `http://`, requests and secrets are unencrypted); redirects are not followed. For binary files: `form: {title: "x", document: {file: "in/scan.pdf"}}` sends multipart/form-data with the bytes of files in `work/`, and `saveTo: "out/doc.pdf"` writes a successful response body to a file in `work/` instead of returning it (up to 100 MB each).
- `fs.readText(path)`, `fs.writeText(path, text)`, `fs.list(path)`, `fs.exists(path)`: paths relative to the `work/` directory (a subdirectory of the harness's directory, created on first use); anything outside it (absolute paths, `..`, symlinks) and `.env*` files are rejected.

A script cannot start a process, but it can save another script with `fs.writeText` and run it in-process with arguments using `runScript(path, args)`:

```js
fs.writeText("greet.js", "console.log('hello', args.join(' ')); return args.length;");
await runScript("greet.js", ["a", "b"]); // prints "hello a b", resolves to 2
```

**Writes:** to change data, the model sets `writes: true` on the call and does all writes of a task in that one call. The whole call needs one approval (the approval card says it changes data and on which hosts, and shows the script). The approval is skipped only if `AGENT_JS_ALLOW_WRITES=1` is set (or the webhook's mode is `allow`) and all the call's hosts are allowed; a host that is not allowed needs approval as before, and the one approval covers both. Requests inside a running script are never confirmed one by one; instead the call's output ends with `requests that changed data:`, one line per request it sent (`PATCH https://... -> 200`, at most 100), so the transcript shows what an approved script really did. In a webhook run the hook's own hosts and writes mode decide, never `AGENT_JS_ALLOWED_HOSTS` / `AGENT_JS_ALLOW_WRITES`.

Each call runs in a fresh runtime with a 256 MB memory limit and a timeout (`timeout_seconds`, default 30, max 300).

Configuration in `.env`:

- `AGENT_JS_ALLOWED_HOSTS=api.github.com,example.com`: calls whose `hosts` are all in this list (or empty) run without asking, as long as they are read-only. A call with `writes: true` still asks for approval, unless `AGENT_JS_ALLOW_WRITES=1` is set. Any other host needs approval for the whole call. This does not depend on `AGENT_ISOLATED`.
- `AGENT_SECRET_<NAME>=<host> <Authorization header value>`, e.g. `AGENT_SECRET_GITHUB=api.github.com Bearer ghp_xxx`: `fetch(url, {secret: "GITHUB"})` makes the harness add that `Authorization` header. The script never sees the value, and the secret is only sent to its host. A server that echoes request headers in its response can still reveal it.
