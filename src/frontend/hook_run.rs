//! Runs of webhooks (see [`crate::hooks`]): a queued [`Job`] becomes a conversation of its
//! own, run to the end without anybody to ask, saved like any other session and optionally
//! reported to the hook's callback URL.
//!
//! A write (`execute_javascript` with `writes: true`) in a hook of mode `approve` pauses the
//! run: the call is stored with the session (`pending`) and the run stops, without the model
//! seeing a refusal. When the user decides, a [`Task::Resume`] job rebuilds the session from
//! disk, runs the call if it was approved and goes on with a message that tells the model
//! what happened.

use super::web::{append, context_event, event_json, tool_event};
use super::{Event, Frontend, Question, ToolCall};
use crate::harness::Session;
use crate::hooks::{Hook, Writes};
use crate::sessions::{Choices, Store};
use crate::tools::skills::{self, SkillFilter};
use futures::future::BoxFuture;
use rig_core::serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// What the history says about the prompt that stopped at a call waiting for approval.
const PAUSED: &str = "Paused: waiting for the user's approval of an execute_javascript call that writes. \
                      The call has not run; the user's decision follows.";

/// What a [`Job`] does.
pub enum Task {
    /// A new run: the hook's template filled from the request.
    Start(String),
    /// Goes on with a run that waits for the user: `pending` is its stored call.
    Resume {
        pending: Value,
        approve: bool,
        comment: Option<String>,
    },
}

/// One thing for the worker to do for a hook.
pub struct Job {
    pub hook: Hook,
    /// The session the run is saved as; it exists (as "queued") from the moment of the request.
    pub session_id: String,
    pub task: Task,
}

/// The transcript the page would show for a run that starts with `prompt`.
pub fn queued_log(hook: &Hook, prompt: &str, note: &str) -> Vec<Value> {
    vec![
        json!({"type": "title", "text": format!("hook: {}", hook.name)}),
        json!({"type": "user", "text": prompt}),
        json!({"type": "note", "text": note}),
    ]
}

/// The run that is working right now (its session id), so that the page can show it while it runs.
pub type Running = Mutex<Option<(String, Arc<Run>)>>;

/// Takes the run off [`Running`] when the job is over, however it ends.
struct Watch<'a>(&'a Running);

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

/// What a run reports to: records the transcript, answers every approval itself.
pub struct Run {
    hook: Hook,
    log: Mutex<Vec<Value>>,
    context: Mutex<Option<Value>>,
    /// The call the run is paused at, once it is.
    pending: Mutex<Option<Value>>,
    /// Signals that `pending` was set; the run is then stopped.
    parked: Notify,
}

impl Run {
    fn new(hook: Hook, log: Vec<Value>, context: Option<Value>) -> Self {
        Self {
            hook,
            log: Mutex::new(log),
            context: Mutex::new(context),
            pending: Mutex::default(),
            parked: Notify::new(),
        }
    }

    /// Stores `call` as what the run waits for and signals the run to stop.
    fn park(&self, call: &ToolCall) {
        let message = self.answer();
        // A writing call without hosts runs on the hook's (see `execute_javascript`): show those.
        let mut args = call.args.clone();
        if args["hosts"].as_array().is_none_or(|h| h.is_empty()) {
            args["hosts"] = json!(self.hook.hosts);
        }
        let mut pending = self.pending.lock().unwrap();
        if pending.is_none() {
            *pending = Some(json!({
                "hook": self.hook.name,
                "message": message,
                "call": {"id": call.id, "name": call.name, "title": call.title, "args": args},
            }));
            self.push(tool_event(call, "waiting"));
            self.parked.notify_one();
        }
    }

    fn push(&self, event: Value) {
        append(&mut self.log.lock().unwrap(), &event);
    }

    /// The transcript so far.
    pub fn snapshot(&self) -> Vec<Value> {
        self.log.lock().unwrap().clone()
    }

    /// The agent's last answer of its own (not a subagent's): the result of the run.
    fn answer(&self) -> String {
        self.log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e["type"] == "text" && e["agent"].is_null())
            .and_then(|e| e["text"].as_str())
            .unwrap_or_default()
            .to_owned()
    }
}

impl Frontend for Run {
    fn emit(&self, event: Event) {
        match event {
            Event::Context(usage) => *self.context.lock().unwrap() = Some(context_event(usage)),
            event => {
                if let Some(json) = event_json(event) {
                    self.push(json);
                }
            }
        }
    }

    /// Nobody is there to ask, so what would need approval is declined, except a script that
    /// only declares hosts the hook allows. One that changes data (`writes: true`) is
    /// declined, run, or paused for the user's decision, as the hook's `writes` says; a
    /// paused call never returns, the run is stopped instead.
    fn approve(&self, call: &ToolCall) -> BoxFuture<'static, bool> {
        let allowed = call.name == "execute_javascript"
            && call.args["hosts"].as_array().is_none_or(|hosts| {
                hosts.iter().all(|h| {
                    h.as_str()
                        .is_some_and(|h| self.hook.hosts.contains(&h.trim().to_ascii_lowercase()))
                })
            });
        let allowed = allowed
            && match (call.args["writes"] == true, self.hook.writes) {
                (false, _) | (true, Writes::Allow) => true,
                (true, Writes::Approve) if call.agent.is_none() => {
                    self.park(call);
                    return Box::pin(futures::future::pending());
                }
                (true, _) => false,
            };
        Box::pin(async move { allowed })
    }

    fn ask(&self, _question: &Question) -> BoxFuture<'static, Option<String>> {
        Box::pin(async { None })
    }
}

/// Runs `job` on a conversation forked from `main` (same model and system prompt, the
/// hook's tools) and saves it. Returns how it ended: `done`, `failed`, `timeout` or
/// `waiting` (paused at a call that needs the user's approval).
pub async fn execute(main: &Session, store: &Store, job: Job, isolated: bool, running: &Running) -> &'static str {
    let (log, context, history) = match &job.task {
        Task::Start(prompt) => (queued_log(&job.hook, prompt, "running"), None, Vec::new()),
        Task::Resume { .. } => match store.load(&job.session_id) {
            Ok(saved) => (saved.log, saved.context, saved.history),
            Err(e) => {
                eprintln!("hook {}: could not resume {}: {e}", job.hook.name, job.session_id);
                return "failed";
            }
        },
    };
    let run = Arc::new(Run::new(job.hook.clone(), log, context));
    *running.lock().unwrap() = Some((job.session_id.clone(), run.clone()));
    let _watch = Watch(running);
    let tools = job.hook.toolset(isolated);
    // What the session shows when it is opened in the page: the hook's tools, and every
    // skill that exists but is not in the hook's list as switched off.
    let choices = Choices {
        tools: Some(tools.names()),
        skills_disabled: Some(
            skills::discover(std::path::Path::new(skills::DIR))
                .skills
                .into_iter()
                .map(|s| s.name)
                .filter(|name| !tools.skills().allows(name))
                .collect(),
        ),
    };
    let skills = tools.skills().clone();
    let mut session = main.fork(run.clone(), tools, &job.hook.note());
    session.set_history(history);
    let save = |session: &Session, run: &Run| {
        let log = run.log.lock().unwrap().clone();
        let context = run.context.lock().unwrap().clone();
        let pending = run.pending.lock().unwrap().clone();
        if let Err(e) = store.save(&job.session_id, session.history(), &log, context.as_ref(), &choices, pending.as_ref()) {
            eprintln!("hook {}: could not save session {}: {e}", job.hook.name, job.session_id);
        }
    };
    // Saved first: for a resumed run this ends the waiting before the approved call runs.
    if matches!(job.task, Task::Resume { .. }) {
        run.push(json!({"type": "note", "text": "running"}));
    }
    save(&session, &run);
    let input = match &job.task {
        Task::Start(prompt) => prompt.clone(),
        Task::Resume { pending, approve, comment } => {
            resume_input(&run, &skills, pending, *approve, comment.as_deref()).await
        }
    };

    let limit = Duration::from_secs(job.hook.timeout_minutes * 60);
    let status = tokio::select! {
        result = tokio::time::timeout(limit, session.prompt(input)) => match result {
            Ok(Ok(())) => "done",
            Ok(Err(error)) => {
                run.push(json!({"type": "error", "text": error}));
                "failed"
            }
            Err(_) => {
                session.commit_stopped();
                run.push(json!({"type": "error", "text": format!("stopped after {} minutes", job.hook.timeout_minutes)}));
                "timeout"
            }
        },
        _ = run.parked.notified() => {
            session.commit_stopped_because(PAUSED);
            run.push(json!({"type": "note", "text": "paused: waiting for the user's approval of the call above"}));
            "waiting"
        }
    };
    // The run is over: no "running" note, and calls that never finished are not shown as running.
    run.log.lock().unwrap().retain(|e| !(e["type"] == "note" && e["text"] == "running"));
    for event in run.log.lock().unwrap().iter_mut() {
        if event["type"] == "tool" && matches!(event["status"].as_str(), Some("pending" | "running")) {
            event["status"] = json!("cancelled");
        }
    }
    if let Some(url) = &job.hook.callback_url {
        let body = json!({
            "hook": job.hook.name,
            "session": job.session_id,
            "status": status,
            "result": run.answer(),
        });
        let note = match post_callback(url, &body).await {
            Ok(()) => "result sent to the callback URL".to_owned(),
            Err(e) => format!("the callback failed: {e}"),
        };
        run.push(json!({"type": "note", "text": note}));
    }
    save(&session, &run);
    status
}

/// The call a run was paused at, as the page shows it.
fn stored_call(pending: &Value) -> ToolCall {
    let call = &pending["call"];
    ToolCall {
        id: call["id"].as_str().unwrap_or("resumed").to_owned(),
        name: call["name"].as_str().unwrap_or("execute_javascript").to_owned(),
        kind: "execute",
        title: call["title"].as_str().unwrap_or_default().to_owned(),
        ask: true,
        args: call["args"].clone(),
        agent: None,
    }
}

/// Records the user's decision in the transcript, runs the call if it was approved, and
/// returns the message that tells the model what happened.
async fn resume_input(run: &Run, skills: &SkillFilter, pending: &Value, approve: bool, comment: Option<&str>) -> String {
    let call = stored_call(pending);
    if !approve {
        run.push(json!({"type": "note", "text": "declined by the user"}));
        run.emit(Event::ToolDenied(call));
        let mut text = String::from("The user declined your execute_javascript call.");
        if let Some(comment) = comment {
            text.push_str(&format!("\nTheir comment: {comment}"));
        }
        return text;
    }
    run.push(json!({"type": "note", "text": "approved by the user"}));
    run.emit(Event::ToolStarted(call.clone()));
    let output = run_call(&run.hook, skills, call.args.clone()).await;
    run.emit(Event::ToolFinished {
        id: call.id,
        kind: call.kind,
        failed: !output.starts_with("ok"),
        output: output.clone(),
    });
    let mut text = format!(
        "The user approved your execute_javascript call and it ran. Output:\n{output}\n\n\
         Check the result. If something failed, fix it (writing again needs approval). \
         Finish with a short report."
    );
    if let Some(comment) = comment {
        text.push_str(&format!("\nTheir comment: {comment}"));
    }
    text
}

/// Runs the approved call with the hook's own settings (its hosts only).
#[cfg(feature = "javascript")]
async fn run_call(hook: &Hook, skills: &SkillFilter, args: Value) -> String {
    crate::tools::execute_javascript::run_approved(args, hook.js_settings(), skills.clone()).await
}

#[cfg(not(feature = "javascript"))]
async fn run_call(_hook: &Hook, _skills: &SkillFilter, _args: Value) -> String {
    "error\nstderr:\nexecute_javascript is not available in this build".to_owned()
}

/// POSTs `body` as JSON to `url`; the answer must be a success.
async fn post_callback(url: &str, body: &Value) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match response.status() {
        s if s.is_success() => Ok(()),
        s => Err(format!("HTTP {s}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn run_with(hosts: &[&str], writes: Writes) -> Run {
        let hook = Hook {
            name: "h".into(),
            token_hash: String::new(),
            template: "t".into(),
            tools: vec![],
            skills: vec![],
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            writes,
            callback_url: None,
            enabled: true,
            timeout_minutes: 1,
        };
        Run::new(hook, Vec::new(), None)
    }

    fn run_for(hosts: &[&str]) -> Run {
        run_with(hosts, Writes::Approve)
    }

    fn call(name: &str, kind: &'static str, args: Value) -> ToolCall {
        ToolCall { id: "1".into(), name: name.into(), kind, title: String::new(), ask: true, args, agent: None }
    }

    #[tokio::test]
    async fn only_scripts_on_the_hooks_hosts_are_approved() {
        let run = run_for(&["api.example.com"]);
        let js = |hosts: Value| call("execute_javascript", "execute", json!({"code": "1", "hosts": hosts}));
        assert!(run.approve(&js(json!(["API.example.com "]))).await);
        assert!(run.approve(&js(json!([]))).await);
        assert!(!run.approve(&js(json!(["api.example.com", "evil.com"]))).await);
        // Everything else is declined.
        assert!(!run.approve(&call("execute", "execute", json!({"command": "ls"}))).await);
        assert!(!run.approve(&call("view_image", "read", json!({"path": "/etc/x.png"}))).await);
        assert!(run.pending.lock().unwrap().is_none());
        assert_eq!(run.ask(&Question { id: "q".into(), question: "?".into(), options: vec![] }).await, None);
    }

    fn writing(hosts: Value) -> ToolCall {
        call("execute_javascript", "execute", json!({"code": "1", "hosts": hosts, "writes": true}))
    }

    #[tokio::test]
    async fn writes_are_declined_run_or_parked_as_the_hook_says() {
        let hosts = json!(["api.example.com"]);
        let deny = run_with(&["api.example.com"], Writes::Deny);
        assert!(!deny.approve(&writing(hosts.clone())).await);
        let allow = run_with(&["api.example.com"], Writes::Allow);
        assert!(allow.approve(&writing(hosts.clone())).await);
        assert!(deny.pending.lock().unwrap().is_none() && allow.pending.lock().unwrap().is_none());

        // Other hosts are declined whatever the mode; so are writes of a subagent.
        let approve = run_with(&["api.example.com"], Writes::Approve);
        assert!(!approve.approve(&writing(json!(["evil.com"]))).await);
        let mut sub = writing(hosts.clone());
        sub.agent = Some("a1".into());
        assert!(!approve.approve(&sub).await);
        assert!(approve.pending.lock().unwrap().is_none());

        // `approve` parks: the call is stored, the run is signalled and the answer never comes.
        assert!(tokio::time::timeout(Duration::from_millis(100), approve.approve(&writing(hosts))).await.is_err());
        let pending = approve.pending.lock().unwrap().clone().unwrap();
        assert_eq!((pending["hook"].as_str(), pending["call"]["id"].as_str()), (Some("h"), Some("1")));
        assert_eq!(pending["call"]["args"]["writes"], true);
        tokio::time::timeout(Duration::from_secs(1), approve.parked.notified()).await.unwrap();
        assert!(approve.log.lock().unwrap().iter().any(|e| e["status"] == "waiting"));
    }
}
