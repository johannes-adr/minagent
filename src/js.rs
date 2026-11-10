use crate::sandbox;
use llrt_modules::module_builder::ModuleBuilder;
use rquickjs::loader::{FileResolver, ScriptLoader};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module};
use std::io;
use std::path::Path;
use std::process::{Output, Stdio};
use tokio::io::AsyncWriteExt;

/// Heap limit of the JS runtime; scripts that allocate more fail with an out-of-memory error.
const MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// `<exe> --js <args...>`, sandboxed where supported.
pub fn command(exe: &Path, args: &[&str], allow_network: bool) -> io::Result<tokio::process::Command> {
    let args = ["--js"].into_iter().chain(args.iter().copied());
    if sandbox::supported() {
        sandbox::command(exe, args, allow_network)
    } else {
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(args);
        Ok(cmd)
    }
}

/// Runs `code` via `<exe> --js -`, sandboxed where supported. The code goes through
/// stdin, not a temp file, so sandboxed processes can't swap it after approval.
/// The child is killed when the returned future is dropped (e.g. on timeout).
pub async fn run_sandboxed(exe: &Path, code: &str, allow_network: bool) -> io::Result<Output> {
    let mut child = command(exe, &["-"], allow_network)?
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(code.as_bytes()).await?;
    drop(stdin);
    child.wait_with_output().await
}

/// Runs a JS module with LLRT's Node/WinterTC APIs (fetch, timers, fs, ...).
/// Relative imports of `.js` files (`import "./x.js"`, also dynamic) resolve against the
/// importing file, or the working directory for stdin.
/// `path` `-` reads the source from stdin. `args` are exposed to the script as `globalThis.ARGS`.
pub async fn run_js(path: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let source = if path == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path)?
    };
    let runtime = AsyncRuntime::new()?;
    runtime.set_memory_limit(MEMORY_LIMIT).await;
    let (resolver, loader, globals) = ModuleBuilder::default().build();
    runtime
        .set_loader(
            (resolver, FileResolver::default()),
            (loader, ScriptLoader::default()),
        )
        .await;
    let context = AsyncContext::full(&runtime).await?;

    context
        .async_with(async |ctx| {
            globals
                .attach(&ctx)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            ctx.globals()
                .set("ARGS", args.to_vec())
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            let promise = Module::evaluate(ctx.clone(), path, source)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| e.to_string())
        })
        .await?;
    // Wait for pending timers, fetches and other spawned jobs.
    runtime.idle().await;
    Ok(())
}
