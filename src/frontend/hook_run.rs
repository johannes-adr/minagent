//! Runs of webhooks (see [`crate::hooks`]): a queued [`Job`] becomes a conversation of its
//! own, run to the end without anybody to ask, saved like any other session and optionally
//! reported to the hook's callback URL.

use super::web::{append, context_event, event_json};
use super::{Event, Frontend, Question, ToolCall};
use crate::harness::Session;
use crate::hooks::Hook;
use crate::sessions::{Choices, Store};
use crate::tools::skills;
use futures::future::BoxFuture;
use rig_core::serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One request to run a hook.
pub struct Job {
    pub hook: Hook,
    /// The session the run is saved as; it exists (as "queued") from the moment of the request.
    pub session_id: String,
    /// The hook's template filled from the request.
    pub prompt: String,
}

/// The transcript the page would show for a run that starts with `prompt`.
pub fn queued_log(hook: &Hook, prompt: &str, note: &str) -> Vec<Value> {
    vec![
        json!({"type": "title", "text": format!("hook: {}", hook.name)}),
        json!({"type": "user", "text": prompt}),
        json!({"type": "note", "text": note}),
    ]
}

/// What a run reports to: records the transcript, answers every approval itself.
struct Run {
    hook: Hook,
    log: Mutex<Vec<Value>>,
    context: Mutex<Option<Value>>,
}

impl Run {
    fn push(&self, event: Value) {
        append(&mut self.log.lock().unwrap(), &event);
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
    /// only declares hosts the hook allows. (Requests that change data are declined too: a
    /// hook that permits them makes the tool run without asking.)
    fn approve(&self, call: &ToolCall) -> BoxFuture<'static, bool> {
        let allowed = call.name == "execute_javascript"
            && call.kind != "fetch"
            && call.args["hosts"].as_array().is_none_or(|hosts| {
                hosts.iter().all(|h| {
                    h.as_str()
                        .is_some_and(|h| self.hook.hosts.contains(&h.trim().to_ascii_lowercase()))
                })
            });
        Box::pin(async move { allowed })
    }

    fn ask(&self, _question: &Question) -> BoxFuture<'static, Option<String>> {
        Box::pin(async { None })
    }
}

/// Runs `job` on a new conversation forked from `main` (same model and system prompt, the
/// hook's tools) and saves it. Returns how it ended: `done`, `failed` or `timeout`.
pub async fn execute(main: &Session, store: &Store, job: Job, isolated: bool) -> &'static str {
    let run = Arc::new(Run {
        hook: job.hook.clone(),
        log: Mutex::new(queued_log(&job.hook, &job.prompt, "running")),
        context: Mutex::new(None),
    });
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
    let mut session = main.fork(run.clone(), tools, &job.hook.note());
    let save = |session: &Session, run: &Run| {
        let log = run.log.lock().unwrap().clone();
        let context = run.context.lock().unwrap().clone();
        if let Err(e) = store.save(&job.session_id, session.history(), &log, context.as_ref(), &choices) {
            eprintln!("hook {}: could not save session {}: {e}", job.hook.name, job.session_id);
        }
    };
    save(&session, &run);

    let limit = Duration::from_secs(job.hook.timeout_minutes * 60);
    let status = match tokio::time::timeout(limit, session.prompt(job.prompt.clone())).await {
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
    use crate::hooks::{Fields, Hooks};

    fn run_for(hosts: &[&str]) -> Run {
        let dir = std::env::temp_dir().join(format!("agent_hook_run_{}", std::process::id()));
        let mut hooks = Hooks::load(dir.join("none.json")).unwrap();
        let body = json!({"name": "h", "template": "t", "hosts": hosts});
        // Never saved: the directory doesn't exist, so keep the hook in memory only.
        let fields = Fields::parse(&body, false).unwrap();
        let hook = match hooks.upsert(fields, || "t".into()) {
            Ok((hook, _)) => hook,
            Err(_) => Hook {
                name: "h".into(),
                token_hash: String::new(),
                template: "t".into(),
                tools: vec![],
                skills: vec![],
                hosts: hosts.iter().map(|h| h.to_string()).collect(),
                allow_writes: false,
                callback_url: None,
                enabled: true,
                timeout_minutes: 1,
            },
        };
        Run { hook, log: Mutex::default(), context: Mutex::default() }
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
        // A request that changes data inside a script is declined, as is everything else.
        assert!(!run.approve(&call("execute_javascript", "fetch", json!({"method": "POST"}))).await);
        assert!(!run.approve(&call("execute", "execute", json!({"command": "ls"}))).await);
        assert!(!run.approve(&call("view_image", "read", json!({"path": "/etc/x.png"}))).await);
        assert_eq!(run.ask(&Question { id: "q".into(), question: "?".into(), options: vec![] }).await, None);
    }
}
