//! The QuickJS runtime of `execute_javascript`: limits, console capture and the host API.
//!
//! The runtime is synchronous and runs on a blocking thread. The script gets no timers and
//! no event loop: promises only settle through other promises or through `fetch`, which
//! blocks until the response is complete. So when the job queue is empty, the script is
//! either done or waits for a promise that can never settle.

use super::modules::{SkillLoader, SkillResolver, skills_root};
use crate::tools::skills::SkillFilter;
use super::{Settings, fetch::Fetch, fs};
use crate::frontend::Frontend;
use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Error, Exception, Function, Module, Object, Result,
    Runtime,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

/// Heap limit of the script; allocating more fails with an out-of-memory error.
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
/// Console output kept per stream; the rest is dropped.
const CAPTURE_LIMIT: usize = 1024 * 1024;
/// Defines `console`, `fetch` and `fs` on top of the native functions.
const PRELUDE: &str = include_str!("prelude.js");

/// What a run produced, ready for `format_output`.
pub(super) struct Outcome {
    pub exit: String,
    pub stdout: String,
    pub stderr: String,
}

/// Console output of a run: `log`/`info` to stdout, `warn`/`error` to stderr.
#[derive(Default)]
struct Output {
    stdout: String,
    stderr: String,
}

impl Output {
    fn push(&mut self, level: &str, line: &str) {
        let stream = match level {
            "warn" | "error" => &mut self.stderr,
            _ => &mut self.stdout,
        };
        if stream.len() < CAPTURE_LIMIT {
            stream.push_str(line);
            stream.push('\n');
        }
    }
}

/// Runs `code` as an ES module (so top-level `await` works) in a fresh runtime. `hosts`
/// are the hostnames `fetch` may contact. Blocks until the script is done or `timeout`
/// has passed; call it from a blocking thread inside a tokio runtime (`handle`).
pub(super) fn run(
    code: &str,
    hosts: Vec<String>,
    confirm_writes: Option<Arc<dyn Frontend>>,
    timeout: Duration,
    settings: Arc<Settings>,
    skills: SkillFilter,
    handle: Handle,
) -> Outcome {
    let deadline = Instant::now() + timeout;
    let output = Rc::new(RefCell::new(Output::default()));
    let fetch = Fetch {
        hosts,
        confirm_writes,
        settings,
        deadline,
        handle,
    };
    let result = super::modules::with_filter(skills, || evaluate(code, deadline, fetch, &output));
    let mut output = output.take();
    let exit = match result {
        Ok(()) => "ok".to_owned(),
        Err(_) if Instant::now() >= deadline => format!(
            "timed out after {}s (raise timeout_seconds if it needs more time). Output so far:",
            timeout.as_secs()
        ),
        Err(message) => {
            output.push("error", &format!("Uncaught {message}"));
            "error".to_owned()
        }
    };
    Outcome {
        exit,
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

/// Runs the script; the error is the exception's message and stack.
fn evaluate(
    code: &str,
    deadline: Instant,
    fetch: Fetch,
    output: &Rc<RefCell<Output>>,
) -> std::result::Result<(), String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_loader(
        SkillResolver,
        SkillLoader {
            root: skills_root(),
        },
    );
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        install(&ctx, fetch, output).catch(&ctx).map_err(describe)?;
        let promise = Module::evaluate(ctx.clone(), "script", code)
            .catch(&ctx)
            .map_err(describe)?;
        match promise.finish::<()>().catch(&ctx) {
            Err(CaughtError::Error(Error::WouldBlock)) => Err(
                "Error: the script awaits a promise that never settles (there are no timers)"
                    .to_owned(),
            ),
            result => result.map_err(describe),
        }
    })
}

/// An uncaught error as `Name: message` followed by the stack (`Display` of the
/// exception would always say `Error`).
fn describe(error: CaughtError<'_>) -> String {
    match error {
        CaughtError::Exception(e) => {
            let name: String = e.as_object().get("name").unwrap_or_else(|_| "Error".into());
            let message = e.message().unwrap_or_default();
            let stack = e.stack().unwrap_or_default();
            format!("{name}: {message}\n{stack}").trim_end().to_owned()
        }
        CaughtError::Value(value) => match value.as_string().map(|s| s.to_string()) {
            Some(Ok(text)) => text,
            _ => format!("{value:?}"),
        },
        CaughtError::Error(error) => error.to_string(),
    }
}

fn throw<'js>(ctx: &Ctx<'js>, message: String) -> Error {
    Exception::throw_message(ctx, &message)
}

/// Installs the globals: the native host functions go into `prelude.js`, which builds
/// `console`, `fetch` and `fs` from them.
fn install<'js>(ctx: &Ctx<'js>, fetch: Fetch, output: &Rc<RefCell<Output>>) -> Result<()> {
    let native = Object::new(ctx.clone())?;
    let output = output.clone();
    native.set(
        "log",
        Function::new(ctx.clone(), move |level: String, line: String| {
            output.borrow_mut().push(&level, &line)
        }),
    )?;
    native.set(
        "fetch",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, url: String, options: String| -> Result<String> {
                fetch.call(&url, &options).map_err(|e| throw(&ctx, e))
            },
        ),
    )?;
    native.set(
        "readText",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, path: String| {
            fs::read_text(&path).map_err(|e| throw(&ctx, e))
        }),
    )?;
    native.set(
        "writeText",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, path: String, text: String| {
            fs::write_text(&path, &text).map_err(|e| throw(&ctx, e))
        }),
    )?;
    native.set(
        "list",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, path: String| {
            fs::list(&path).map_err(|e| throw(&ctx, e))
        }),
    )?;
    native.set(
        "exists",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, path: String| {
            fs::exists(&path).map_err(|e| throw(&ctx, e))
        }),
    )?;
    let setup: Function = ctx.eval(PRELUDE)?;
    setup.call::<_, ()>((native,))
}
