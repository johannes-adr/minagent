//! The `execute_javascript` tool (feature `javascript`): runs a script in an in-process
//! QuickJS runtime. The script can only use the host API of [`runtime`]: `console`, `fetch`
//! (to declared hosts, with secrets the script never sees), `fs` (working directory
//! only, plus read-only `skill:` paths) and `import` of skill modules. The sandbox is this
//! capability set itself, so it needs no container.

mod fetch;
mod fs;
mod modules;
mod runtime;

use crate::harness::{Approval, Context, ToolPolicy};
use crate::tools::process::format_output;
use rig_agent::rig_tool;
use rig_core::tool::ToolExecutionError;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::runtime::Handle;

/// Default and maximum `timeout_seconds` of `execute_javascript`.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;

/// A credential bound to one host (`AGENT_SECRET_<NAME>=<host> <header value>`).
pub struct Secret {
    pub host: String,
    /// The `Authorization` header value, e.g. `Bearer ghp_xxx`.
    pub value: String,
}

/// What the configuration gives the tool.
#[derive(Default)]
pub struct Settings {
    /// Hosts whose calls run without asking (`AGENT_JS_ALLOWED_HOSTS`), lowercase.
    pub allowed_hosts: Vec<String>,
    /// Secrets by name (the `<NAME>` of `AGENT_SECRET_<NAME>`).
    pub secrets: HashMap<String, Secret>,
}

static SETTINGS: OnceLock<Arc<Settings>> = OnceLock::new();

/// Sets the tool's configuration. Call once at startup, before creating a session.
pub fn configure(settings: Settings) {
    let _ = SETTINGS.set(Arc::new(settings));
}

fn settings() -> Arc<Settings> {
    SETTINGS.get_or_init(Default::default).clone()
}

/// Trimmed, lowercase, non-empty hostnames.
fn normalize(hosts: Option<&[String]>) -> Vec<String> {
    hosts
        .unwrap_or_default()
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// Calls that only contact allowed hosts (or none) run without asking; the rest needs
/// approval. The container is irrelevant: the script's capabilities are what is checked.
impl ToolPolicy for ExecuteJavascript {
    const KIND: &'static str = "execute";

    fn prompt(_context: &Context) -> String {
        let mut secrets: Vec<_> = settings()
            .secrets
            .iter()
            .map(|(name, s)| format!("`{name}` (only for {})", s.host))
            .collect();
        secrets.sort();
        let secrets = if secrets.is_empty() {
            "No secrets are configured.".to_owned()
        } else {
            format!("Available secrets: {}.", secrets.join(", "))
        };
        format!(
            "Use `execute_javascript` for workflow automation: calling APIs and transforming data. \
The script is an ES module (top-level `await` works) and has only these globals: \
`console.log/info/warn/error`; `fetch(url, {{method, headers, body, secret, form, saveTo}})` returning a promise of \
`{{status, ok, headers, text(), json()}}` (`body` is a string, use `JSON.stringify`; response `headers` is a plain \
object with lowercase keys; redirects are not followed). For binary files: `form: {{name: value}}` sends \
multipart/form-data instead of `body`, where a value is a string, a number, `{{file: \"<path in work>\", filename?, type?}}` \
(the file's bytes) or an array of those (repeated field); `saveTo: \"<path in work>\"` writes a successful (2xx) \
response body to that file instead (binary-safe; `text()` is then empty and `saved` is its size in bytes); the synchronous \
`fs.readText(path)`, `fs.writeText(path, text)`, `fs.list(path)`, `fs.exists(path)`, whose paths are relative \
to the `work` directory (created on first use; `.env*` files are not accessible), except that the read \
functions also accept `skill:<name>/<path>` for the files of a skill (read-only, writing fails); and \
`await runScript(path, args = [])`, which runs a script saved with `fs.writeText`. The saved script is a function \
body (no `import`/`export`) that sees `args`, may `await`, and whose `return` value is the result. \
Each call starts fresh; only files in `work` persist between calls. There are no \
timers, no `require`, no npm packages, no `import` except `import {{ f }} from \"skill:<name>/<path>.js\"` of the \
JavaScript modules a skill documents (read-only; relative imports work inside the skill), no process or environment access and no way to start a process. \
`fetch` only reaches `https://` and `http://` URLs of hosts you list in \
the `hosts` argument (the user must approve hosts that are not pre-approved). \
`secret: \"NAME\"` makes `fetch` add the `Authorization` header from the configured secret; the script never \
sees its value, and it only works for the host the secret is bound to. {secrets} \
Print results with `console.log`; thrown errors are returned as output."
        )
    }

    fn approval(args: &ExecuteJavascriptParameters, _context: &Context) -> Approval {
        approval(args, &settings().allowed_hosts)
    }
}

fn approval(args: &ExecuteJavascriptParameters, allowed: &[String]) -> Approval {
    let hosts = normalize(args.hosts.as_deref());
    if hosts.iter().all(|h| allowed.contains(h)) {
        let mut lines = args.code.lines();
        let first = lines.next().unwrap_or_default();
        let more = if lines.next().is_some() { " ..." } else { "" };
        return Approval::auto(format!("[javascript] {first}{more}"));
    }
    Approval::ask(format!(
        "execute_javascript with network access to {}:\n{}",
        hosts.join(", "),
        args.code
    ))
}

/// Run a JavaScript (ES module) script in a sandboxed in-process runtime and return its
/// console output and, if it threw, the error. For workflow automation: calling APIs
/// with `fetch` and transforming data. Only `console`, `fetch`, `fs` and `runScript` exist; there is
/// no `require`, no npm and no process access. `import` works only for skill modules
/// (`import { f } from "skill:<name>/<path>.js"`), and the `fs` read functions also accept
/// read-only `skill:<name>/<path>` paths.
#[rig_tool(params(
    code = "The script (ES module; top-level await works). Print results with console.log.",
    hosts = "Hostnames the script will contact with fetch, e.g. [\"api.github.com\"]. fetch to any other host fails; omit when the script does no network access.",
    timeout_seconds = "Stop the script after this many seconds (default 30, max 300)."
))]
pub(crate) async fn execute_javascript(
    code: String,
    hosts: Option<Vec<String>>,
    timeout_seconds: Option<u64>,
) -> Result<String, ToolExecutionError> {
    let timeout = Duration::from_secs(
        timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    );
    run(code, normalize(hosts.as_deref()), timeout, settings())
        .await
        .map_err(ToolExecutionError::from_error)
}

/// Runs the script on a blocking thread and formats its outcome like `execute`'s output.
async fn run(
    code: String,
    hosts: Vec<String>,
    timeout: Duration,
    settings: Arc<Settings>,
) -> Result<String, tokio::task::JoinError> {
    let handle = Handle::current();
    let outcome =
        tokio::task::spawn_blocking(move || runtime::run(&code, hosts, timeout, settings, handle))
            .await?;
    Ok(format_output(
        &outcome.exit,
        outcome.stdout.as_bytes(),
        outcome.stderr.as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn approval_of(args: &str, allowed: &[&str]) -> Approval {
        let args = rig_core::serde_json::from_str(args).unwrap();
        let allowed: Vec<String> = allowed.iter().map(|h| h.to_string()).collect();
        approval(&args, &allowed)
    }

    #[test]
    fn only_calls_to_allowed_hosts_run_without_asking() {
        let allowed = ["api.github.com"];
        assert!(!approval_of(r#"{"code":"1"}"#, &[]).ask);
        assert!(!approval_of(r#"{"code":"1","hosts":[]}"#, &[]).ask);
        let github = r#"{"code":"1","hosts":["API.github.com "]}"#;
        assert!(!approval_of(github, &allowed).ask);
        assert!(approval_of(github, &[]).ask);
        let both = r#"{"code":"1","hosts":["api.github.com","evil.com"]}"#;
        assert!(approval_of(both, &allowed).ask);
    }

    #[test]
    fn approval_titles() {
        assert_eq!(
            approval_of(r#"{"code":"let a = 1;\nconsole.log(a)"}"#, &[]),
            Approval::auto("[javascript] let a = 1; ...")
        );
        assert_eq!(
            approval_of(r#"{"code":"one line"}"#, &[]),
            Approval::auto("[javascript] one line")
        );
        assert_eq!(
            approval_of(r#"{"code":"await f();\nx","hosts":["a.com","b.com"]}"#, &[]),
            Approval::ask("execute_javascript with network access to a.com, b.com:\nawait f();\nx")
        );
    }

    #[test]
    fn approval_does_not_depend_on_the_container() {
        let args = rig_core::serde_json::from_str(r#"{"code":"1","hosts":["x.com"]}"#).unwrap();
        for isolated in [true, false] {
            assert!(ExecuteJavascript::approval(&args, &Context { isolated }).ask);
        }
    }

    fn secret(host: &str, value: &str) -> Secret {
        Secret {
            host: host.to_owned(),
            value: value.to_owned(),
        }
    }

    async fn js_with(code: &str, hosts: &[&str], secrets: &[(&str, Secret)], secs: u64) -> String {
        let settings = Settings {
            secrets: secrets
                .iter()
                .map(|(name, s)| (name.to_string(), secret(&s.host, &s.value)))
                .collect(),
            ..Settings::default()
        };
        let hosts = hosts.iter().map(|h| h.to_string()).collect();
        run(
            code.to_owned(),
            hosts,
            Duration::from_secs(secs),
            Arc::new(settings),
        )
        .await
        .unwrap()
    }

    async fn js(code: &str) -> String {
        js_with(code, &[], &[], 10).await
    }

    #[tokio::test]
    async fn console_output_is_captured() {
        let out = js(r#"
            console.log("hello", 1 + 1, {a: [1, 2]}, [null], undefined);
            console.info("info");
            console.warn("careful");
            console.error(new Error("bad").message);
            const v = await Promise.resolve(42);
            console.log("awaited", v);
        "#)
        .await;
        assert_eq!(
            out,
            "ok\nstdout:\nhello 2 {\"a\":[1,2]} [null] undefined\ninfo\nawaited 42\nstderr:\ncareful\nbad"
        );
        assert_eq!(js("").await, "ok\n(no output)");
    }

    #[tokio::test]
    async fn a_thrown_error_is_reported_with_the_output_so_far() {
        let out = js("console.log('before'); throw new Error('boom');").await;
        assert!(
            out.starts_with("error\nstdout:\nbefore\nstderr:\nUncaught Error: boom"),
            "{out}"
        );
        let out = js("await Promise.reject(new TypeError('late'))").await;
        assert!(out.contains("Uncaught TypeError: late"), "{out}");
        let out = js("let = ;").await;
        assert!(out.contains("SyntaxError"), "{out}");
    }

    #[tokio::test]
    async fn only_the_documented_globals_exist() {
        let out = js(r#"
            for (const name of ["require", "process", "Deno", "Bun", "native", "setTimeout", "XMLHttpRequest"]) {
                if (typeof globalThis[name] !== "undefined") console.log("leaked", name);
            }
            console.log(typeof fetch, typeof fs.readText, typeof console.log);
        "#)
        .await;
        assert_eq!(out, "ok\nstdout:\nfunction function function");
        let out = js("import fs from 'node:fs';").await;
        assert!(out.starts_with("error"), "{out}");
        assert!(out.contains("only `skill:<name>/<path>` imports"), "{out}");
        let out = js("await new Promise(() => {});").await;
        assert!(out.contains("never settles"), "{out}");
    }

    /// A throwaway skill in the real skills directory (like the tests of `work/`).
    struct TestSkill(std::path::PathBuf, String);

    impl TestSkill {
        fn new(tag: &str) -> Self {
            let name = format!("jstest-{tag}-{}", std::process::id());
            let dir = modules::skills_root().join(&name);
            std::fs::create_dir_all(dir.join("assets")).unwrap();
            std::fs::write(dir.join("assets/a.txt"), "asset").unwrap();
            std::fs::write(dir.join("lib.js"), "export const twice = (n) => n * 2;").unwrap();
            Self(dir, name)
        }
    }

    impl Drop for TestSkill {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn skills_are_importable_and_read_only() {
        let skill = TestSkill::new("import");
        let name = &skill.1;
        let out = js(&format!(r#"
            import {{ twice }} from "skill:{name}/lib.js";
            console.log(twice(21), fs.readText("skill:{name}/assets/a.txt"));
            console.log(fs.exists("skill:{name}/lib.js"), fs.exists("skill:{name}/nope"));
            console.log(JSON.stringify(fs.list("skill:{name}")));
            for (const op of [
                () => fs.writeText("skill:{name}/lib.js", "x"),
                () => fs.writeText("skill:{name}/new.js", "x"),
                () => fs.writeText("skill:{name}/../x.txt", "x"),
            ]) {{
                try {{ op(); console.log("written"); }} catch (e) {{ console.log(e.message); }}
            }}
            for (const path of ["skill:../x", "skill:{name}/../../work/x", "skill:{name}/.env", "skill:/etc/passwd"]) {{
                try {{ fs.readText(path); console.log("read", path); }} catch (e) {{ console.log("blocked"); }}
            }}
        "#))
        .await;
        assert!(!out.contains("written") && !out.contains("read "), "{out}");
        assert_eq!(out.matches("skills are read-only").count(), 3, "{out}");
        assert_eq!(out.matches("blocked").count(), 4, "{out}");
        assert!(
            out.contains("42 asset\ntrue false\n[\"assets/\",\"lib.js\"]"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(skill.0.join("lib.js")).unwrap(),
            "export const twice = (n) => n * 2;"
        );
        assert!(!skill.0.join("new.js").exists());
    }

    #[tokio::test]
    async fn only_skill_imports_work_in_the_main_script() {
        for code in [
            "import 'lodash';",
            "import './x.js';",
            "import 'https://example.com/x.js';",
            "import '/etc/passwd';",
        ] {
            let out = js(code).await;
            assert!(out.starts_with("error"), "{code}: {out}");
            assert!(
                out.contains("only `skill:<name>/<path>` imports"),
                "{code}: {out}"
            );
        }
    }

    #[tokio::test]
    async fn an_endless_loop_times_out() {
        let started = Instant::now();
        let out = js_with("console.log('start'); while (true) {}", &[], &[], 1).await;
        assert!(out.starts_with("timed out after 1s"), "{out}");
        assert!(out.contains("start"), "{out}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn memory_is_limited() {
        let out = js("const a = []; while (true) a.push(new Array(1e6).fill(1));").await;
        assert!(out.starts_with("error"), "{out}");
        assert!(out.to_lowercase().contains("out of memory"), "{out}");
    }

    #[tokio::test]
    async fn fs_is_confined_to_the_working_directory() {
        let out = js(r#"
            for (const path of ["../x", "/etc/passwd", "a/../../x", ".env", "sub/.env.local"]) {
                for (const op of [() => fs.readText(path), () => fs.writeText(path, "x"), () => fs.exists(path)]) {
                    try { op(); console.log("allowed", path); } catch (e) { console.log("blocked", path); }
                }
            }
            try { fs.readText("../x"); } catch (e) { console.log(e.message); }
        "#)
        .await;
        assert!(!out.contains("allowed"), "{out}");
        assert_eq!(out.matches("blocked").count(), 15, "{out}");
        assert!(out.contains("stay inside it"), "{out}");
    }

    #[tokio::test]
    async fn fs_reads_writes_and_lists() {
        let dir = format!("js_fs_test_{}", std::process::id());
        let out = js(&format!(r#"
            fs.writeText("{dir}/sub/a.txt", "héllo");
            console.log(fs.readText("{dir}/sub/a.txt"), fs.exists("{dir}/sub/a.txt"), fs.exists("{dir}/nope"));
            console.log(JSON.stringify(fs.list("{dir}")), JSON.stringify(fs.list("{dir}/sub")));
        "#))
        .await;
        let on_disk = crate::tools::workdir().join(&dir);
        let written = std::fs::read_to_string(on_disk.join("sub/a.txt"));
        let _ = std::fs::remove_dir_all(&on_disk);
        assert_eq!(out, "ok\nstdout:\nhéllo true false\n[\"sub/\"] [\"a.txt\"]");
        assert_eq!(written.unwrap(), "héllo");
    }

    /// The pattern from the tool prompt: save a script, run it later with arguments.
    #[tokio::test]
    async fn a_saved_script_runs_with_arguments() {
        let name = format!("js_script_test_{}.js", std::process::id());
        let out = js(&format!(r#"
            fs.writeText("{name}", "const v = await Promise.resolve(args.join('+')); console.log('hello', v); return v.length;");
            console.log(await runScript("{name}", ["a", "b"]));
            console.log(await runScript("{name}"));
        "#))
        .await;
        let _ = std::fs::remove_file(crate::tools::workdir().join(&name));
        assert_eq!(out, "ok\nstdout:\nhello a+b\n3\nhello \n0");
    }

    #[tokio::test]
    async fn fetch_to_an_undeclared_host_is_rejected() {
        let code = r#"
            try { await fetch("https://example.com/"); console.log("fetched"); }
            catch (e) { console.log("rejected:", e.message); }
        "#;
        let out = js(code).await;
        assert!(
            out.contains("rejected: host example.com was not declared"),
            "{out}"
        );
        let out = js_with(code, &["other.com"], &[], 10).await;
        assert!(
            out.contains("rejected: host example.com was not declared"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn a_secret_is_not_sent_to_another_host() {
        let secrets = [("GITHUB", secret("api.github.com", "Bearer ghp_x"))];
        let code = r#"
            try { await fetch("https://other.com/", { secret: "GITHUB" }); console.log("sent"); }
            catch (e) { console.log("rejected:", e.message); }
        "#;
        let out = js_with(code, &["other.com"], &secrets, 10).await;
        assert!(out.contains("may only be sent to api.github.com"), "{out}");
        assert!(!out.contains("ghp_x"), "{out}");
    }

    /// Answers one request with `{"ok":true}` and returns the request it received.
    async fn serve_once() -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nX-Reply: yes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (port, task)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_adds_the_secret_without_showing_it_to_the_script() {
        let (port, server) = serve_once().await;
        let secrets = [("TEST", secret("127.0.0.1", "Bearer s3cret"))];
        let code = format!(
            r#"
            const r = await fetch("http://127.0.0.1:{port}/v1/items", {{
                method: "post",
                headers: {{ "Authorization": "Bearer fake", "X-Custom": "1" }},
                body: JSON.stringify({{ name: "a" }}),
                secret: "TEST",
            }});
            console.log(r.status, r.ok, r.headers["x-reply"], JSON.stringify(await r.json()));
        "#
        );
        let out = js_with(&code, &["127.0.0.1"], &secrets, 10).await;
        let request = server.await.unwrap().to_lowercase();
        assert_eq!(out, "ok\nstdout:\n201 true yes {\"ok\":true}");
        assert!(!out.contains("s3cret"), "{out}");
        assert!(request.starts_with("post /v1/items http/1.1"), "{request}");
        assert!(
            request.contains("authorization: bearer s3cret\r\n"),
            "{request}"
        );
        assert!(!request.contains("bearer fake"), "{request}");
        assert!(request.contains("x-custom: 1\r\n"), "{request}");
    }

    /// Answers one request with `status` and `body`; returns the raw request, body included.
    async fn serve_bytes(status: &str, body: Vec<u8>) -> (u16, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status.to_owned();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            let header_end = loop {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse().unwrap());
            while request.len() < header_end + length {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            request
        });
        (port, task)
    }

    /// Bytes that are not valid UTF-8, so a text round trip would corrupt them.
    const BINARY: &[u8] = b"%PDF-1.4\n\xff\xfe\x00\x80binary\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_uploads_a_file_as_multipart_form() {
        let dir = format!("js_form_test_{}", std::process::id());
        let on_disk = crate::tools::workdir().join(&dir);
        std::fs::create_dir_all(&on_disk).unwrap();
        std::fs::write(on_disk.join("scan.pdf"), BINARY).unwrap();
        let (port, server) = serve_bytes("200 OK", br#""task-uuid""#.to_vec()).await;
        let code = format!(
            r#"
            const r = await fetch("http://127.0.0.1:{port}/upload/", {{
                method: "POST",
                form: {{ title: "Scan", tags: [1, 2], document: {{ file: "{dir}/scan.pdf" }} }},
            }});
            console.log(r.status, await r.json());
        "#
        );
        let out = js_with(&code, &["127.0.0.1"], &[], 10).await;
        let request = server.await.unwrap();
        let _ = std::fs::remove_dir_all(&on_disk);
        assert_eq!(out, "ok\nstdout:\n200 task-uuid");
        let text = String::from_utf8_lossy(&request);
        assert!(
            text.contains("content-type: multipart/form-data; boundary="),
            "{text}"
        );
        assert!(
            text.contains(
                "name=\"document\"; filename=\"scan.pdf\"\r\nContent-Type: application/pdf\r\n\r\n"
            ),
            "{text}"
        );
        assert!(request.windows(BINARY.len()).any(|w| w == BINARY), "{text}");
        assert_eq!(text.matches("name=\"tags\"").count(), 2, "{text}");
        assert!(text.contains("name=\"title\"\r\n\r\nScan\r\n"), "{text}");
    }

    #[tokio::test]
    async fn form_files_must_be_in_the_working_directory() {
        for path in ["../Cargo.toml", "/etc/passwd", ".env", "skill:x/../../.env"] {
            let code = format!(
                r#"try {{ await fetch("http://127.0.0.1:9/", {{ method: "POST", form: {{ f: {{ file: "{path}" }} }} }}); }}
                catch (e) {{ console.log("rejected:", e.message); }}"#
            );
            let out = js_with(&code, &["127.0.0.1"], &[], 10).await;
            assert!(out.contains("rejected:"), "{path}: {out}");
            assert!(!out.contains("[package]"), "{path}: {out}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_saves_a_binary_response_to_a_file() {
        let dir = format!("js_save_test_{}", std::process::id());
        let (port, server) = serve_bytes("200 OK", BINARY.to_vec()).await;
        let code = format!(
            r#"
            const r = await fetch("http://127.0.0.1:{port}/doc", {{ saveTo: "{dir}/doc.pdf" }});
            console.log(r.status, r.saved, JSON.stringify(await r.text()), fs.exists("{dir}/doc.pdf"));
        "#
        );
        let out = js_with(&code, &["127.0.0.1"], &[], 10).await;
        server.await.unwrap();
        let on_disk = crate::tools::workdir().join(&dir);
        let saved = std::fs::read(on_disk.join("doc.pdf"));
        let _ = std::fs::remove_dir_all(&on_disk);
        assert_eq!(out, format!("ok\nstdout:\n200 {} \"\" true", BINARY.len()));
        assert_eq!(saved.unwrap(), BINARY);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_error_response_is_returned_instead_of_saved() {
        let dir = format!("js_save_error_test_{}", std::process::id());
        let (port, server) = serve_bytes(
            "404 Not Found",
            br#"{"detail":"No Document matches"}"#.to_vec(),
        )
        .await;
        let code = format!(
            r#"
            const r = await fetch("http://127.0.0.1:{port}/doc", {{ saveTo: "{dir}/doc.pdf" }});
            console.log(r.status, r.saved, await r.text(), fs.exists("{dir}/doc.pdf"));
            try {{ await fetch("http://127.0.0.1:{port}/", {{ saveTo: "skill:x/a" }}); }}
            catch (e) {{ console.log(e.message); }}
        "#
        );
        let out = js_with(&code, &["127.0.0.1"], &[], 10).await;
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(crate::tools::workdir().join(&dir));
        assert_eq!(
            out,
            "ok\nstdout:\n404 undefined {\"detail\":\"No Document matches\"} false\n\"skill:x/a\": skills are read-only"
        );
    }
}
