//! Web frontend: `agent --web [addr]` serves a chat page (default `127.0.0.1:8080`).
//!
//! The page is a Svelte app (`web/`), built to static files that are embedded in the binary
//! and served from `/`. The API lives under `/api`: the page gets the session as a stream of
//! JSON events over server-sent events (`GET /api/events`) and talks back with
//! `POST /api/prompt`, `POST /api/approve`, `POST /api/answer` and `POST /api/cancel`; webhook
//! runs that wait for a write to be approved are listed by `GET /api/approvals` and decided by
//! `POST /api/approvals/decide`. Like the terminal UI there is one
//! session; every browser tab shows it, and a reloaded tab gets the whole transcript replayed.
//!
//! Whoever can call the API can make the agent run commands, so all of it needs a token
//! (header `x-token`, or `?token=` for the event stream): the one a login returns (see
//! `auth`: one password, set on the page the first time) or the random one printed at
//! startup. Only the static page and the login endpoints are served without it.

use crate::frontend::{ContextUsage, Event, Frontend, Question, ToolCall};
use crate::config::Config;
use crate::frontend::auth::{self, Auth, LoginError, SetupError};
use crate::frontend::files::{Area, FileError};
use crate::frontend::hook_run::{self, Job, Task};
use crate::hooks::{self, Fields, Hooks};
use crate::tools::skills;
use crate::update;
use crate::harness::{self, Context, Session, Toolset};
use crate::sessions::{self, Choices, Store};
use crate::thinking::{self, Thinking};
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use rig_core::serde_json::{self, Value, json};
use rust_embed::RustEmbed;
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Notify, broadcast, mpsc, oneshot};

/// The receiving end of what the browser asks for, see [`Web::new`].
pub type Inbox = mpsc::UnboundedReceiver<Command>;

/// The built UI (`npm run build` in `web/`), embedded into the binary. Debug builds read
/// the folder from disk instead, so a rebuilt UI shows up without recompiling.
#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

pub const DEFAULT_ADDR: &str = "127.0.0.1:8080";
/// Tool output sent to the browser is cut to this many bytes (the model still gets all of it).
const MAX_OUTPUT: usize = 20_000;

#[derive(Default)]
struct Inner {
    /// Every event so far, replayed to browsers that connect later. Adjacent answer
    /// fragments are merged.
    log: Vec<Value>,
    /// Tool calls waiting for the user, by call id.
    approvals: HashMap<String, oneshot::Sender<bool>>,
    /// Questions of the agent waiting for the user, by question id.
    questions: HashMap<String, oneshot::Sender<String>>,
    /// The latest context usage; only this one is replayed, after the log.
    context: Option<Value>,
    /// The stored session being shown; assigned when its first prompt runs.
    session_id: Option<String>,
    /// The thinking levels the model offers, if it offers any.
    thinking: Option<Thinking>,
    /// The thinking level the user picked; `None` leaves it to the model's default.
    think: Option<String>,
    /// The tools the configuration enables (`None`: all) and the ones the shown session
    /// uses instead, if it has its own choice.
    default_tools: Option<Vec<String>>,
    /// The skills the configuration disables.
    default_disabled_skills: Vec<String>,
    /// What the shown session uses instead of the defaults.
    choices: Choices,
}

/// What the worker does, one at a time.
pub enum Command {
    Prompt(String),
    /// Shows a stored session, or a new one for `None`.
    Switch(Option<String>),
    Delete(String),
    /// Gives the shown session its own tools; `None` goes back to the configured ones.
    SetTools(Option<Vec<String>>),
    /// Gives the shown session its own set of disabled skills; `None` goes back to the configured ones.
    SetSkills(Option<Vec<String>>),
    /// Applies a changed configuration (already saved to the settings file).
    Reload(Box<Config>),
    /// Replaces the program with this release and starts it again.
    Update(String),
}

/// The web frontend's state, shared by the harness side ([`Frontend`]) and the HTTP handlers.
pub struct Web {
    /// The token printed at startup; valid like a session token of the login.
    token: String,
    auth: Auth,
    inner: Mutex<Inner>,
    live: broadcast::Sender<Value>,
    busy: AtomicBool,
    commands: mpsc::UnboundedSender<Command>,
    store: Store,
    cancel: Notify,
    /// The settings file the settings page edits.
    config_path: PathBuf,
    /// Set when skills were edited: the agent is rebuilt before the next prompt, so that
    /// its system prompt lists them.
    skills_dirty: AtomicBool,
    /// The webhooks (`hooks.json` next to the settings file) and the runs waiting for the worker.
    hooks: Mutex<Hooks>,
    jobs: Mutex<VecDeque<Job>>,
    jobs_ready: Notify,
    /// The newest release, asked for at most once per hour.
    latest: update::Latest,
}

impl Web {
    /// Returns the frontend and the receiving end of the prompts the browser submits,
    /// which [`run`] feeds to the session.
    pub fn new(
        store: Store,
        thinking: Option<Thinking>,
        config_path: PathBuf,
    ) -> io::Result<(Arc<Self>, Inbox)> {
        let (commands, inbox) = mpsc::unbounded_channel();
        let web = Self {
            token: random_token()?,
            auth: Auth::load(auth::path_for(&config_path))?,
            inner: Mutex::new(Inner {
                thinking,
                ..Inner::default()
            }),
            live: broadcast::channel(4096).0,
            busy: AtomicBool::new(false),
            commands,
            store,
            cancel: Notify::new(),
            config_path: config_path.clone(),
            skills_dirty: AtomicBool::new(false),
            hooks: Mutex::new(Hooks::load(hooks::path_for(&config_path)).map_err(io::Error::other)?),
            jobs: Mutex::default(),
            jobs_ready: Notify::new(),
            latest: update::Latest::default(),
        };
        Ok((Arc::new(web), inbox))
    }

    /// Records `event` and sends it to the connected browsers.
    fn push(&self, event: Value) {
        let mut inner = self.inner.lock().unwrap();
        append(&mut inner.log, &event);
        // Sent under the lock so that `subscribe` sees each event exactly once.
        let _ = self.live.send(event);
    }

    /// Sends the context usage to the connected browsers and keeps it for later ones.
    fn set_context(&self, usage: ContextUsage) {
        let event = context_event(usage);
        let mut inner = self.inner.lock().unwrap();
        inner.context = Some(event.clone());
        let _ = self.live.send(event);
    }

    /// The thinking slider's state, if the model has levels.
    fn thinking_event(&self, inner: &Inner) -> Option<Value> {
        let thinking = inner.thinking.as_ref()?;
        Some(json!({
            "type": "thinking",
            "levels": thinking.levels,
            "default": thinking.default,
            "current": inner.think.as_deref().unwrap_or(&thinking.default),
        }))
    }

    /// The tools of the shown session, for the page's tool menu.
    fn tools_event(inner: &Inner) -> Value {
        let enabled = Toolset::only(inner.choices.tools.clone().or_else(|| inner.default_tools.clone()));
        json!({
            "type": "tools",
            "available": harness::tool_names(),
            "enabled": enabled.names(),
            "default": Toolset::only(inner.default_tools.clone()).names(),
            "custom": inner.choices.tools.is_some(),
        })
    }

    /// The skills of the shown session, for the page's skill menu: every skill that exists
    /// and which of them the session may not use.
    fn skills_event(inner: &Inner) -> Value {
        let available: Vec<Value> = skills::discover(std::path::Path::new(skills::DIR))
            .skills
            .into_iter()
            .map(|s| json!({"name": s.name, "description": s.description}))
            .collect();
        let disabled = inner
            .choices
            .skills_disabled
            .clone()
            .unwrap_or_else(|| inner.default_disabled_skills.clone());
        json!({
            "type": "skills",
            "available": available,
            "disabled": disabled,
            "default": inner.default_disabled_skills,
            "custom": inner.choices.skills_disabled.is_some(),
        })
    }

    /// Tells the pages which skills exist (after the file browser changed some).
    fn skills_changed(&self) {
        let inner = self.inner.lock().unwrap();
        let _ = self.live.send(Self::skills_event(&inner));
    }

    /// Sets what the configuration enables, for sessions without their own choice: the
    /// tools (`None`: all) and the skills that are switched off.
    pub fn set_defaults(&self, tools: Option<Vec<String>>, disabled_skills: Vec<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.default_tools = tools;
        inner.default_disabled_skills = disabled_skills;
        let _ = self.live.send(Self::tools_event(&inner));
        let _ = self.live.send(Self::skills_event(&inner));
    }

    /// The `think` value for the next prompt; `None` while the user hasn't chosen.
    fn think_param(&self) -> Option<Value> {
        let level = self.inner.lock().unwrap().think.clone()?;
        Some(thinking::param(&level))
    }

    /// The events so far and a receiver for all later ones.
    fn subscribe(&self) -> (Vec<Value>, broadcast::Receiver<Value>) {
        let inner = self.inner.lock().unwrap();
        let mut events = vec![session_event(&inner.session_id)];
        events.extend(self.thinking_event(&inner));
        events.push(Self::tools_event(&inner));
        events.push(Self::skills_event(&inner));
        events.extend(inner.log.clone());
        events.extend(inner.context.clone());
        (events, self.live.subscribe())
    }

    /// Shows another session: the open pages clear their transcript and get this one.
    fn replace(
        &self,
        id: Option<String>,
        log: Vec<Value>,
        context: Option<Value>,
        choices: Choices,
    ) {
        let mut inner = self.inner.lock().unwrap();
        inner.session_id = id;
        inner.log = log;
        inner.context = context;
        inner.choices = choices;
        let _ = self.live.send(session_event(&inner.session_id));
        let _ = self.live.send(Self::tools_event(&inner));
        let _ = self.live.send(Self::skills_event(&inner));
        for event in inner.log.iter().chain(&inner.context) {
            let _ = self.live.send(event.clone());
        }
    }

    /// Tells the open pages to reload the session list.
    fn sessions_changed(&self) {
        let _ = self.live.send(json!({"type": "sessions"}));
    }

    /// Writes the current session to disk (without busy markers, which are only live). A
    /// webhook run that waited for approval stops waiting: whoever continues it here takes it over.
    fn save(&self, history: &[rig_core::message::Message]) -> Result<(), String> {
        let (id, log, context, choices) = {
            let mut inner = self.inner.lock().unwrap();
            let id = inner
                .session_id
                .get_or_insert_with(|| self.store.new_id())
                .clone();
            let log: Vec<Value> = inner
                .log
                .iter()
                .filter(|e| e["type"] != "status")
                .cloned()
                .map(|mut e| {
                    if e["status"] == "waiting" {
                        e["status"] = json!("cancelled");
                    }
                    e
                })
                .collect();
            (id, log, inner.context.clone(), inner.choices.clone())
        };
        self.store
            .save(&id, history, &log, context.as_ref(), &choices, None)
            .map_err(|e| format!("could not save session {id}: {e}"))
    }

    /// Answers a pending approval; `false` if there is none with this id.
    fn answer(&self, id: &str, allow: bool) -> bool {
        let reply = self.inner.lock().unwrap().approvals.remove(id);
        reply.is_some_and(|reply| reply.send(allow).is_ok())
    }

    /// Answers a pending question; `false` if there is none with this id.
    fn answer_question(&self, id: &str, answer: &str) -> bool {
        let reply = self.inner.lock().unwrap().questions.remove(id);
        let sent = reply.is_some_and(|reply| reply.send(answer.to_owned()).is_ok());
        if sent {
            self.push(
                json!({"type": "question", "id": id, "status": "answered", "answer": answer}),
            );
        }
        sent
    }

    /// Marks every unanswered approval and question as cancelled (the prompt that asked is gone).
    fn cancel_approvals(&self) {
        let (ids, questions): (Vec<String>, Vec<String>) = {
            let mut inner = self.inner.lock().unwrap();
            (
                inner.approvals.drain().map(|(id, _)| id).collect(),
                inner.questions.drain().map(|(id, _)| id).collect(),
            )
        };
        for id in ids {
            self.push(json!({"type": "tool", "id": id, "status": "cancelled"}));
        }
        for id in questions {
            self.push(json!({"type": "question", "id": id, "status": "cancelled"}));
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
        self.push(json!({"type": "status", "busy": busy}));
    }
}

fn session_event(id: &Option<String>) -> Value {
    json!({"type": "session", "id": id})
}

/// Calls that were still waiting or running when a session was saved never finished.
fn settle(log: &mut [Value]) {
    for event in log {
        if matches!(event["type"].as_str(), Some("tool" | "question"))
            && matches!(event["status"].as_str(), Some("pending" | "running"))
        {
            event["status"] = json!("cancelled");
        }
    }
}

/// 128 random bits as hex, from the operating system.
pub(super) fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Cuts `text` to [`MAX_OUTPUT`] bytes at a character boundary.
pub(super) fn clip(mut text: String) -> String {
    if text.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let rest = text.len() - end;
        text.truncate(end);
        text.push_str(&format!("\n… ({rest} more bytes not shown)"));
    }
    text
}

/// The arguments with long strings (e.g. a script) cut like tool output.
fn clip_args(args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), clip_args(value)))
                .collect(),
        ),
        Value::String(text) => Value::String(clip(text.clone())),
        other => other.clone(),
    }
}

/// The event describing `call`; the page merges events with the same `id`.
pub(super) fn tool_event(call: &ToolCall, status: &str) -> Value {
    json!({
        "type": "tool",
        "id": call.id,
        "name": call.name,
        "kind": call.kind,
        // The title can be shortened by the tool (e.g. a script's first line); the
        // arguments are what the model actually sent.
        "title": call.title,
        "args": clip_args(&call.args),
        "status": status,
        // Which subagent's panel shows the call; none means the main transcript.
        "agent": call.agent,
    })
}

/// Adds `event` to a transcript; adjacent answer or reasoning fragments of the same agent
/// are merged.
pub(super) fn append(log: &mut Vec<Value>, event: &Value) {
    if matches!(event["type"].as_str(), Some("text" | "reasoning"))
        && let Some(last) = log.last_mut()
        && last["type"] == event["type"]
        && last["agent"] == event["agent"]
        && let Some(Value::String(text)) = last.get_mut("text")
    {
        text.push_str(event["text"].as_str().unwrap_or_default());
    } else {
        log.push(event.clone());
    }
}

/// The page's event for a context usage.
pub(super) fn context_event(usage: ContextUsage) -> Value {
    json!({
        "type": "context",
        "tokens": usage.tokens,
        "window": usage.window,
        "percent": usage.percent(),
        "text": usage.to_string(),
    })
}

/// The page's event for what the harness reports; `None` for context usage (see
/// [`context_event`]) and for the parts of a subagent's report the page doesn't show.
pub(super) fn event_json(event: Event) -> Option<Value> {
    Some(match event {
        Event::Text(text) => json!({"type": "text", "text": text}),
        Event::Reasoning(text) => json!({"type": "reasoning", "text": text}),
        Event::Note(text) => json!({"type": "note", "text": text}),
        Event::ToolStarted(call) => tool_event(&call, "running"),
        Event::ToolDenied(call) => tool_event(&call, "denied"),
        Event::ToolFinished {
            id, output, failed, ..
        } => json!({
            "type": "tool",
            "id": id,
            "status": if failed { "failed" } else { "done" },
            "output": clip(output),
        }),
        Event::Context(_) => return None,
        Event::SubagentStarted {
            agent,
            description,
            prompt,
        } => json!({
            "type": "subagent",
            "agent": agent,
            "description": description,
            "prompt": clip(prompt),
            "status": "running",
        }),
        Event::SubagentFinished { agent, failed } => json!({
            "type": "subagent",
            "agent": agent,
            "status": if failed { "failed" } else { "done" },
        }),
        // The subagent's context is not the one the header shows.
        Event::Sub { agent, event } => match *event {
            Event::Text(text) => json!({"type": "text", "agent": agent, "text": text}),
            Event::Reasoning(text) => json!({"type": "reasoning", "agent": agent, "text": text}),
            Event::Note(text) => json!({"type": "note", "agent": agent, "text": text}),
            _ => return None,
        },
    })
}

impl Frontend for Web {
    fn emit(&self, event: Event) {
        match event {
            Event::Context(usage) => self.set_context(usage),
            event => {
                if let Some(json) = event_json(event) {
                    self.push(json);
                }
            }
        }
    }

    fn approve(&self, call: &ToolCall) -> BoxFuture<'static, bool> {
        let (reply, answer) = oneshot::channel();
        self.inner
            .lock()
            .unwrap()
            .approvals
            .insert(call.id.clone(), reply);
        self.push(tool_event(call, "pending"));
        Box::pin(async move { answer.await == Ok(true) })
    }

    fn ask(&self, question: &Question) -> BoxFuture<'static, Option<String>> {
        let (reply, answer) = oneshot::channel();
        self.inner
            .lock()
            .unwrap()
            .questions
            .insert(question.id.clone(), reply);
        self.push(json!({
            "type": "question",
            "id": question.id,
            "question": question.question,
            "options": question.options,
            "status": "pending",
        }));
        Box::pin(async move { answer.await.ok() })
    }
}

/// Constant-time comparison, so the token can't be guessed byte by byte.
fn token_matches(given: &str, token: &str) -> bool {
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// The token a request carries: header `x-token`, or `?token=` for the event stream.
fn request_token<'a>(headers: &'a HeaderMap, uri: &'a Uri) -> Option<&'a str> {
    headers
        .get("x-token")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            uri.query()?
                .split('&')
                .find_map(|pair| pair.strip_prefix("token="))
        })
}

impl Web {
    /// Is `given` the startup token or the token of a login?
    fn token_valid(&self, given: &str) -> bool {
        token_matches(given, &self.token) || self.auth.valid(given)
    }
}

async fn authorize(State(web): State<Arc<Web>>, request: Request, next: Next) -> Response {
    if request_token(request.headers(), request.uri()).is_some_and(|given| web.token_valid(given)) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// `GET /api/auth`: whether there is a password yet and whether the request's token is good.
async fn auth_status(State(web): State<Arc<Web>>, headers: HeaderMap, uri: Uri) -> Json<Value> {
    let authenticated = request_token(&headers, &uri).is_some_and(|given| web.token_valid(given));
    Json(json!({"configured": web.auth.is_set(), "authenticated": authenticated}))
}

/// `POST /api/auth/setup`: sets the first password (`409` once there is one) and logs in.
async fn auth_setup(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    match web.auth.setup(body["password"].as_str().unwrap_or_default()).await {
        Ok(token) => Json(json!({"token": token})).into_response(),
        Err(SetupError::Exists) => error(StatusCode::CONFLICT, "a password is set already"),
        Err(SetupError::Busy) => too_many_attempts(),
        Err(SetupError::Empty) => error(StatusCode::BAD_REQUEST, "the password can't be empty"),
        Err(SetupError::Failed(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `POST /api/auth/login`: `{"password"}` for a session token, `401` if it is wrong.
async fn auth_login(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    match web.auth.login(body["password"].as_str().unwrap_or_default()).await {
        Ok(token) => Json(json!({"token": token})).into_response(),
        Err(LoginError::Wrong) => error(StatusCode::UNAUTHORIZED, "wrong password"),
        Err(LoginError::Busy) => too_many_attempts(),
    }
}

fn too_many_attempts() -> Response {
    error(StatusCode::TOO_MANY_REQUESTS, "too many attempts at once: wait a moment")
}

async fn auth_logout(State(web): State<Arc<Web>>, headers: HeaderMap, uri: Uri) -> StatusCode {
    if let Some(given) = request_token(&headers, &uri) {
        web.auth.logout(given);
    }
    StatusCode::NO_CONTENT
}

/// Serves the built UI. Paths without a file extension fall back to `index.html`, so the
/// UI can have routes of its own.
async fn static_file(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    let file = Assets::get(path).or_else(|| {
        let name = path.rsplit('/').next().unwrap_or(path);
        (!name.contains('.')).then(|| Assets::get("index.html")).flatten()
    });
    let Some(file) = file else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Built assets have the hash of their content in the name; the page itself must be refetched.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

async fn events(
    State(web): State<Arc<Web>>,
) -> Sse<impl Stream<Item = Result<sse::Event, Infallible>>> {
    let (log, receiver) = web.subscribe();
    // If a browser falls so far behind that events were dropped, its stream ends and it
    // reconnects, which replays the transcript.
    let live = futures::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.ok().map(|event| (event, receiver))
    });
    let stream = futures::stream::iter(log)
        .chain(live)
        .map(|event| Ok(sse::Event::default().data(event.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn prompt(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let text = body["text"].as_str().unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    // One prompt at a time, like the terminal UI.
    if web.busy.swap(true, Ordering::SeqCst) {
        return StatusCode::CONFLICT;
    }
    web.push(json!({"type": "user", "text": text}));
    web.push(json!({"type": "status", "busy": true}));
    if web.commands.send(Command::Prompt(text)).is_err() {
        web.set_busy(false);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::ACCEPTED
}

async fn approve(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let id = body["id"].as_str().unwrap_or_default();
    if web.answer(id, body["allow"] == true) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

/// `{"id": "q0", "answer": "..."}` answers a question of the agent.
async fn answer(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let id = body["id"].as_str().unwrap_or_default();
    let answer = body["answer"].as_str().unwrap_or_default();
    if !answer.trim().is_empty() && web.answer_question(id, answer.trim()) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

/// `{"level": "xhigh"}` picks the thinking level for the next prompts.
async fn set_thinking(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let level = body["level"].as_str().unwrap_or_default();
    let mut inner = web.inner.lock().unwrap();
    let Some(thinking) = &inner.thinking else {
        return StatusCode::NOT_FOUND;
    };
    if !thinking.levels.iter().any(|l| l == level) {
        return StatusCode::BAD_REQUEST;
    }
    inner.think = Some(level.to_owned());
    // Sent under the lock like the other state events.
    if let Some(event) = web.thinking_event(&inner) {
        let _ = web.live.send(event);
    }
    StatusCode::NO_CONTENT
}

/// `{"tools": ["execute", ...]}` gives the shown session its own tools, `{"tools": null}`
/// goes back to the configured ones.
async fn set_tools(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let tools = match &body["tools"] {
        Value::Null => None,
        Value::Array(names) => {
            let names: Option<Vec<String>> =
                names.iter().map(|n| n.as_str().map(str::to_owned)).collect();
            match names {
                Some(names) if harness::validate_tools(&names).is_ok() => Some(names),
                _ => return StatusCode::BAD_REQUEST,
            }
        }
        _ => return StatusCode::BAD_REQUEST,
    };
    queue(&web, Command::SetTools(tools))
}

/// Runs a file operation on the area named by the body's `root` (`skills` or `work`).
fn file_op(
    web: &Web,
    body: &Value,
    changes: bool,
    op: impl FnOnce(&Area, &Value) -> Result<Value, FileError>,
) -> Response {
    let root = body["root"].as_str().unwrap_or_default();
    let Some(area) = Area::named(root) else {
        return error(StatusCode::BAD_REQUEST, "root must be skills or work");
    };
    match op(&area, body) {
        Ok(value) => {
            if changes && root == "skills" {
                web.skills_dirty.store(true, Ordering::SeqCst);
                web.skills_changed();
            }
            Json(value).into_response()
        }
        Err(e) => error(
            StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            e.message(),
        ),
    }
}

fn path_of(body: &Value) -> &str {
    body["path"].as_str().unwrap_or_default()
}

/// `{"root": "skills", "path": "dir"}`: the entries of a directory.
async fn files_list(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    file_op(&web, &body, false, |area, body| {
        Ok(json!({"entries": area.list(path_of(body))?}))
    })
}

/// `{"root", "path"}`: the text of a file, and whether it is read-only.
async fn files_read(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    file_op(&web, &body, false, |area, body| {
        let (content, readonly) = area.read(path_of(body))?;
        Ok(json!({"content": content, "readonly": readonly}))
    })
}

/// `{"root", "path", "content"}` writes a file, creating missing directories.
async fn files_write(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    file_op(&web, &body, true, |area, body| {
        let content = body["content"].as_str().ok_or_else(|| FileError::Invalid("content must be text".into()))?;
        area.write(path_of(body), content)?;
        Ok(json!({}))
    })
}

async fn files_mkdir(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    file_op(&web, &body, true, |area, body| {
        area.mkdir(path_of(body))?;
        Ok(json!({}))
    })
}

async fn files_delete(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    file_op(&web, &body, true, |area, body| {
        area.remove(path_of(body))?;
        Ok(json!({}))
    })
}

/// `{"root", "from", "to"}` moves or renames a file or directory.
async fn files_rename(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    file_op(&web, &body, true, |area, body| {
        let part = |key: &str| body[key].as_str().unwrap_or_default().to_owned();
        area.rename(&part("from"), &part("to"))?;
        Ok(json!({}))
    })
}

/// Queues a run of `hook` for `prompt`. The session exists at once, as "queued", so the
/// caller can be told its id. `Err` when too many runs are waiting.
fn enqueue(web: &Web, hook: hooks::Hook, prompt: String) -> Result<(String, usize), StatusCode> {
    let mut jobs = web.jobs.lock().unwrap();
    if jobs.len() >= hooks::MAX_QUEUE {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let session_id = web.store.new_id();
    let log = hook_run::queued_log(&hook, &prompt, "queued");
    web.store
        .save(&session_id, &[], &log, None, &Choices::default(), None)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    jobs.push_back(Job {
        hook,
        session_id: session_id.clone(),
        task: Task::Start(prompt),
    });
    let position = jobs.len();
    drop(jobs);
    web.jobs_ready.notify_one();
    web.sessions_changed();
    Ok((session_id, position))
}

/// The token of a webhook request: `Authorization: Bearer <token>`, `X-Hook-Token` or `?token=`.
fn hook_token(headers: &HeaderMap, query: Option<&str>) -> String {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| header("x-hook-token"))
        .or_else(|| query?.split('&').find_map(|pair| pair.strip_prefix("token=")))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// `POST /hooks/<name>`: an outside service starts the agent. Needs the hook's own token,
/// not the page's. Answers `202 {"session": id, "queued": n}`; the result is the session's
/// (and goes to the hook's callback URL, if it has one). `429` when too many runs wait.
async fn trigger_hook(
    State(web): State<Arc<Web>>,
    Path(name): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = hook_token(&headers, query.as_deref());
    let hook = web.hooks.lock().unwrap().get(&name).cloned();
    // An unknown hook and a wrong token look the same.
    let Some(hook) = hook.filter(|h| h.check_token(&token)) else {
        return error(StatusCode::UNAUTHORIZED, "unknown hook or wrong token");
    };
    if !hook.enabled {
        return error(StatusCode::FORBIDDEN, "this hook is switched off");
    }
    let raw = String::from_utf8_lossy(&body);
    let json: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    // The token itself must never reach the model.
    let shown: Vec<(String, String)> = headers
        .iter()
        .filter(|(n, _)| !matches!(n.as_str(), "authorization" | "x-hook-token" | "cookie"))
        .filter_map(|(n, v)| Some((n.as_str().to_owned(), v.to_str().ok()?.to_owned())))
        .collect();
    let prompt = hooks::render(
        &hook.template,
        &hooks::Request {
            hook: &hook.name,
            raw: &raw,
            body: &json,
            headers: &shown,
        },
    );
    match enqueue(&web, hook, prompt) {
        Ok((session, queued)) => (
            StatusCode::ACCEPTED,
            Json(json!({"session": session, "queued": queued})),
        )
            .into_response(),
        Err(status) => error(status, "too many runs are waiting"),
    }
}

/// The hooks and what the form needs to know (never the tokens).
async fn hooks_list(State(web): State<Arc<Web>>) -> Json<Value> {
    let hooks = web.hooks.lock().unwrap();
    Json(json!({
        "hooks": hooks.list().iter().map(hooks::Hook::to_public).collect::<Vec<_>>(),
        "isolated": Context::from_env().isolated,
        "tools": harness::tool_names().into_iter().filter(|t| *t != "ask_user").collect::<Vec<_>>(),
        "skills": Web::skills_event(&web.inner.lock().unwrap())["available"].clone(),
        "queued": web.jobs.lock().unwrap().len(),
        "max_queue": hooks::MAX_QUEUE,
    }))
}

/// Creates a hook (`"create": true`, answers with its token once) or changes one's settings.
async fn hooks_save(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    let fields = match Fields::parse(&body, Context::from_env().isolated) {
        Ok(fields) => fields,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let mut hooks = web.hooks.lock().unwrap();
    let exists = fields.name.as_deref().is_some_and(|n| hooks.get(n).is_some());
    if body["create"] == true && exists {
        return error(StatusCode::CONFLICT, "a hook with this name exists");
    }
    if body["create"] != true && !exists {
        return error(StatusCode::NOT_FOUND, "no such hook");
    }
    let new_token = || random_token().unwrap_or_default();
    match hooks.upsert(fields, new_token) {
        Ok((hook, token)) => Json(json!({"hook": hook.to_public(), "token": token})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Gives a hook a new token; the old one stops working.
async fn hooks_token(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or_default();
    let token = random_token().unwrap_or_default();
    match web.hooks.lock().unwrap().renew_token(name, token) {
        Ok(token) => Json(json!({"token": token})).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, e),
    }
}

async fn hooks_delete(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or_default();
    match web.hooks.lock().unwrap().remove(name) {
        Ok(()) => Json(json!({})).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, e),
    }
}

/// The webhook runs waiting for the user's decision, oldest first, as the approvals page
/// shows them. A run whose decision is queued no longer counts.
async fn approvals_list(State(web): State<Arc<Web>>) -> Json<Value> {
    let decided: Vec<String> = web
        .jobs
        .lock()
        .unwrap()
        .iter()
        .filter(|job| matches!(job.task, Task::Resume { .. }))
        .map(|job| job.session_id.clone())
        .collect();
    let approvals: Vec<Value> = web
        .store
        .waiting()
        .into_iter()
        .filter(|s| !decided.iter().any(|id| *id == s["id"]))
        .map(|s| {
            let (pending, call) = (&s["pending"], &s["pending"]["call"]);
            json!({
                "session": s["id"],
                "hook": pending["hook"],
                "time": s["updated"],
                "message": pending["message"],
                "title": call["title"],
                "hosts": call["args"]["hosts"],
                "code": call["args"]["code"],
            })
        })
        .collect();
    Json(json!({"approvals": approvals}))
}

/// `{"session": id, "approve": true, "comment": "..."}` decides on the call a webhook run
/// waits at. The run goes on in the worker, not here. `404` for an unknown session, `409`
/// if it isn't waiting (or already decided) or its hook is gone or switched off.
async fn approvals_decide(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    let Some(approve) = body["approve"].as_bool() else {
        return error(StatusCode::BAD_REQUEST, "approve must be true or false");
    };
    let comment = body["comment"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned);
    let Some(id) = body["session"].as_str().filter(|id| sessions::valid_id(id)) else {
        return error(StatusCode::BAD_REQUEST, "a session id is needed");
    };
    let Ok(saved) = web.store.load(id) else {
        return error(StatusCode::NOT_FOUND, "no such session");
    };
    let Some(pending) = saved.pending else {
        return error(StatusCode::CONFLICT, "this session is not waiting for approval");
    };
    let name = pending["hook"].as_str().unwrap_or_default();
    let hook = match web.hooks.lock().unwrap().get(name).cloned() {
        None => return error(StatusCode::CONFLICT, format!("the hook {name} was deleted, so its run can't continue")),
        Some(hook) if !hook.enabled => {
            return error(StatusCode::CONFLICT, format!("the hook {name} is switched off: switch it on to decide"));
        }
        Some(hook) => hook,
    };
    let mut jobs = web.jobs.lock().unwrap();
    if jobs.iter().any(|job| job.session_id == id) {
        return error(StatusCode::CONFLICT, "this session was decided already");
    }
    if jobs.len() >= hooks::MAX_QUEUE {
        return error(StatusCode::TOO_MANY_REQUESTS, "too many runs are waiting");
    }
    jobs.push_back(Job { hook, session_id: id.to_owned(), task: Task::Resume { pending, approve, comment } });
    drop(jobs);
    web.jobs_ready.notify_one();
    web.sessions_changed();
    (StatusCode::ACCEPTED, Json(json!({}))).into_response()
}

/// `{"disabled": ["paperless"]}` switches those skills off for the shown session,
/// `{"disabled": null}` goes back to the configured ones.
async fn set_skills(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let disabled = match &body["disabled"] {
        Value::Null => None,
        Value::Array(names) => match names.iter().map(|n| n.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>() {
            Some(names) => Some(names),
            None => return StatusCode::BAD_REQUEST,
        },
        _ => return StatusCode::BAD_REQUEST,
    };
    queue(&web, Command::SetSkills(disabled))
}

fn config_json(web: &Web, config: &Config) -> Value {
    json!({
        "path": web.config_path.display().to_string(),
        "entries": config.entries(),
        "problems": config.problems(),
        "tools": harness::tool_names(),
        "skills": skills::discover(std::path::Path::new(skills::DIR)).skills.into_iter().map(|s| s.name).collect::<Vec<_>>(),
    })
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error": message.into()}))).into_response()
}

/// Every setting with its value (secrets only as "is set"), and what is wrong with them.
async fn get_config(State(web): State<Arc<Web>>) -> Response {
    match Config::load_from(&web.config_path) {
        Ok(config) => Json(config_json(&web, &config)).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `{"LLM_MODEL": "x", "OPENAI_API_KEY": null}` saves those settings to the settings file
/// (`null` removes one) and applies them to the agent, which takes effect for the next
/// prompt. Refused while the agent works, and for an invalid configuration.
async fn put_config(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> Response {
    let Some(changes) = body.as_object() else {
        return error(StatusCode::BAD_REQUEST, "expected a JSON object");
    };
    if web.busy.load(Ordering::SeqCst) {
        return error(StatusCode::CONFLICT, "wait for the agent to finish");
    }
    let config = match Config::save_changes(&web.config_path, changes) {
        Ok(config) => config,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let json = config_json(&web, &config);
    match queue(&web, Command::Reload(Box::new(config))) {
        StatusCode::ACCEPTED => Json(json).into_response(),
        other => error(other, "saved, but the agent was busy: restart it to apply"),
    }
}

async fn list_sessions(State(web): State<Arc<Web>>) -> Json<Value> {
    let current = web.inner.lock().unwrap().session_id.clone();
    Json(json!({"current": current, "sessions": web.store.list()}))
}

/// Takes the one-at-a-time slot and queues `command`.
fn queue(web: &Web, command: Command) -> StatusCode {
    if web.busy.swap(true, Ordering::SeqCst) {
        return StatusCode::CONFLICT;
    }
    web.push(json!({"type": "status", "busy": true}));
    if web.commands.send(command).is_err() {
        web.set_busy(false);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::ACCEPTED
}

/// `{"id": "..."}` opens that session, no id starts a new one.
async fn open_session(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    let id = match body["id"].as_str() {
        Some(id) if sessions::valid_id(id) => Some(id.to_owned()),
        Some(_) => return StatusCode::BAD_REQUEST,
        None => None,
    };
    queue(&web, Command::Switch(id))
}

async fn delete_session(State(web): State<Arc<Web>>, Json(body): Json<Value>) -> StatusCode {
    match body["id"].as_str() {
        Some(id) if sessions::valid_id(id) => queue(&web, Command::Delete(id.to_owned())),
        _ => StatusCode::BAD_REQUEST,
    }
}

async fn cancel(State(web): State<Arc<Web>>) -> StatusCode {
    if web.busy.load(Ordering::SeqCst) {
        web.cancel.notify_waiters();
    }
    StatusCode::NO_CONTENT
}

fn router(web: Arc<Web>) -> Router {
    let api = Router::new()
        .route("/events", get(events))
        .route("/prompt", post(prompt))
        .route("/approve", post(approve))
        .route("/answer", post(answer))
        .route("/cancel", post(cancel))
        .route("/thinking", post(set_thinking))
        .route("/tools", post(set_tools))
        .route("/skills", post(set_skills))
        .route("/config", get(get_config).put(put_config))
        .route("/hooks", get(hooks_list))
        .route("/hooks/save", post(hooks_save))
        .route("/hooks/token", post(hooks_token))
        .route("/hooks/delete", post(hooks_delete))
        .route("/version", get(version))
        .route("/update", post(start_update))
        .route("/approvals", get(approvals_list))
        .route("/approvals/decide", post(approvals_decide))
        .route("/files/list", post(files_list))
        .route("/files/read", post(files_read))
        .route("/files/write", post(files_write))
        .route("/files/mkdir", post(files_mkdir))
        .route("/files/delete", post(files_delete))
        .route("/files/rename", post(files_rename))
        .route("/sessions", get(list_sessions))
        .route("/session", post(open_session))
        .route("/session/delete", post(delete_session))
        .layer(middleware::from_fn_with_state(web.clone(), authorize))
        // Added after the layer: the login must be reachable without a token.
        .route("/auth", get(auth_status))
        .route("/auth/setup", post(auth_setup))
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        .with_state(web.clone());
    Router::new()
        .nest("/api", api)
        // Outside the page's token: a hook has its own.
        .route("/hooks/{name}", post(trigger_hook).with_state(web))
        .fallback(static_file)
}

/// Runs what the browser asks for, one at a time. A cancelled prompt stays in
/// the session's history as its input plus a digest of what it did.
async fn worker(web: Arc<Web>, mut session: Session, mut inbox: Inbox) {
    loop {
        // What the user asks for comes before waiting webhook runs.
        let command = match inbox.try_recv() {
            Ok(command) => command,
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {
                let job = web.jobs.lock().unwrap().pop_front();
                if let Some(job) = job {
                    hook_run::execute(&session, &web.store, job, Context::from_env().isolated).await;
                    web.sessions_changed();
                    continue;
                }
                tokio::select! {
                    command = inbox.recv() => match command {
                        Some(command) => command,
                        None => break,
                    },
                    _ = web.jobs_ready.notified() => continue,
                }
            }
        };
        run_command(&web, &mut session, command).await;
    }
}

/// Runs one thing the browser asked for.
async fn run_command(web: &Arc<Web>, session: &mut Session, command: Command) {
    match command {
        Command::Prompt(input) => {
            if web.skills_dirty.swap(false, Ordering::SeqCst) {
                session.refresh();
            }
            session.set_thinking(web.think_param());
            tokio::select! {
                result = session.prompt(input) => {
                    if let Err(error) = result {
                        web.push(json!({"type": "error", "text": error}));
                    }
                }
                _ = web.cancel.notified() => {
                    session.commit_stopped();
                    web.cancel_approvals();
                    web.push(json!({"type": "note", "text": "stopped"}));
                }
            }
            if let Err(error) = web.save(session.history()) {
                web.push(json!({"type": "error", "text": error}));
            }
            web.sessions_changed();
        }
        Command::Switch(id) => switch(web, session, id),
        Command::SetTools(tools) => {
            session.set_tools(tools.clone());
            let saved = {
                let mut inner = web.inner.lock().unwrap();
                inner.choices.tools = tools;
                let _ = web.live.send(Web::tools_event(&inner));
                inner.session_id.is_some()
            };
            // A conversation that has not started has nothing to save yet.
            if saved && let Err(error) = web.save(session.history()) {
                web.push(json!({"type": "error", "text": error}));
            }
        }
        Command::SetSkills(disabled) => {
            session.set_skills(disabled.clone());
            let saved = {
                let mut inner = web.inner.lock().unwrap();
                inner.choices.skills_disabled = disabled;
                let _ = web.live.send(Web::skills_event(&inner));
                inner.session_id.is_some()
            };
            if saved && let Err(error) = web.save(session.history()) {
                web.push(json!({"type": "error", "text": error}));
            }
        }
        Command::Reload(config) => reload(web, session, &config).await,
        Command::Update(version) => update(web, &version).await,
        Command::Delete(id) => {
            let current = web.inner.lock().unwrap().session_id.clone();
            match web.store.delete(&id) {
                Ok(()) if current.as_deref() == Some(id.as_str()) => {
                    switch(web, session, None)
                }
                Ok(()) => {}
                Err(error) => web.push(
                    json!({"type": "error", "text": format!("could not delete {id}: {error}")}),
                ),
            }
            web.sessions_changed();
        }
    }
    web.set_busy(false);
}

/// `GET /api/version`: the running version, the newest release and whether this process can
/// install it.
async fn version(State(web): State<Arc<Web>>) -> Json<Value> {
    let (latest, error) = if update::is_dev_build() {
        (None, None)
    } else {
        match web.latest.get().await {
            Ok(latest) => (Some(latest), None),
            Err(error) => (None, Some(error)),
        }
    };
    let newer = latest.as_deref().is_some_and(|l| update::is_newer(l, update::VERSION));
    let blocker = update::blocker(Context::from_env().isolated);
    Json(json!({
        "version": update::VERSION,
        "dev": update::is_dev_build(),
        "latest": latest,
        "newer": newer,
        "can_update": newer && blocker.is_none(),
        "blocker": if newer { blocker } else { None },
        "error": error,
    }))
}

/// `POST /api/update`: installs the newest release and restarts. Answers `202` at once; the
/// page waits for the new process to answer. `409` when it cannot be done now.
async fn start_update(State(web): State<Arc<Web>>) -> Response {
    if update::is_dev_build() {
        return error(StatusCode::CONFLICT, "this is a build from source, not a release");
    }
    if let Some(blocker) = update::blocker(Context::from_env().isolated) {
        return error(StatusCode::CONFLICT, blocker);
    }
    let latest = match web.latest.get().await {
        Ok(latest) if update::is_newer(&latest, update::VERSION) => latest,
        Ok(_) => return error(StatusCode::CONFLICT, "this is the newest version"),
        Err(e) => return error(StatusCode::BAD_GATEWAY, e),
    };
    if !web.jobs.lock().unwrap().is_empty() {
        return error(StatusCode::CONFLICT, "webhook runs are waiting; update when they are done");
    }
    match queue(&web, Command::Update(latest)) {
        StatusCode::ACCEPTED => StatusCode::ACCEPTED.into_response(),
        other => error(other, "the agent is busy; update when it is done"),
    }
}

/// Installs `version` and starts it in place of this process. The worker is idle while this
/// runs, so nothing is lost, except webhook runs still waiting for it: those are not started
/// again after a restart, so the update is refused while there are any.
async fn update(web: &Web, version: &str) {
    if !web.jobs.lock().unwrap().is_empty() {
        let text = "webhook runs are waiting; update when they are done";
        web.push(json!({"type": "error", "text": text}));
        return;
    }
    web.push(json!({"type": "note", "text": format!("updating to {version}")}));
    match update::install(version).await {
        Ok(exe) => {
            web.push(json!({"type": "note", "text": "restarting"}));
            // Lets the page receive the note before the connection goes away.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let text = update::restart(&exe);
            web.push(json!({"type": "error", "text": text}));
        }
        Err(e) => web.push(json!({"type": "error", "text": format!("update failed: {e}")})),
    }
}

/// Applies a changed configuration: JavaScript settings, model, tools, thinking levels.
async fn reload(web: &Web, session: &mut Session, config: &Config) {
    #[cfg(feature = "javascript")]
    if let Ok(settings) = config.javascript() {
        crate::tools::execute_javascript::configure(settings);
    }
    match config.model_unchecked() {
        Ok(model) => session.reconfigure(
            model,
            config.context_window().ok().flatten(),
            config.tools(),
            config.disabled_skills(),
        ),
        Err(error) => return web.push(json!({"type": "error", "text": error})),
    }
    // The levels belong to the model, so the choice of the old one is dropped.
    let thinking = match config.ollama() {
        Some((url, key, model)) => Thinking::discover(&url, key.as_deref(), &model).await,
        None => None,
    };
    let mut inner = web.inner.lock().unwrap();
    inner.thinking = thinking;
    inner.think = None;
    inner.default_tools = config.tools();
    inner.default_disabled_skills = config.disabled_skills();
    let event = web
        .thinking_event(&inner)
        .unwrap_or_else(|| json!({"type": "thinking", "levels": []}));
    let _ = web.live.send(event);
    let _ = web.live.send(Web::tools_event(&inner));
    let _ = web.live.send(Web::skills_event(&inner));
}

/// Makes `id` (or a fresh session) the one the agent continues and the pages show.
fn switch(web: &Web, session: &mut Session, id: Option<String>) {
    let Some(id) = id else {
        session.set_history(Vec::new());
        session.set_tools(None);
        session.set_skills(None);
        web.replace(None, Vec::new(), None, Choices::default());
        return;
    };
    match web.store.load(&id) {
        Ok(saved) => {
            let mut log = saved.log;
            settle(&mut log);
            session.set_history(saved.history);
            session.set_tools(saved.choices.tools.clone());
            session.set_skills(saved.choices.skills_disabled.clone());
            web.replace(Some(id), log, saved.context, saved.choices);
        }
        Err(error) => web.push(json!({"type": "error", "text": error})),
    }
}

/// Serves the page on `addr` until Ctrl-C. `session` must have been created with `web`
/// as its frontend and `inbox` is the receiver from [`Web::new`].
pub async fn run(web: Arc<Web>, inbox: Inbox, session: Session, addr: &str) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let port = listener.local_addr()?.port();
    println!("web UI: http://localhost:{port}/#token={}", web.token);
    tokio::spawn(worker(web.clone(), session, inbox));
    // Not a graceful shutdown: that would wait for the open event streams.
    tokio::select! {
        result = axum::serve(listener, router(web)) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn temp_store() -> Store {
        use std::sync::atomic::AtomicUsize;
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("agent_web_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    fn turn(event: MockStreamEvent) -> Vec<MockStreamEvent> {
        vec![event, MockStreamEvent::final_response_with_total_tokens(1)]
    }

    /// A server whose model answers with `turns`.
    async fn start(turns: Vec<Vec<MockStreamEvent>>) -> (Arc<Web>, SocketAddr) {
        start_with(MockCompletionModel::from_stream_turns(turns)).await
    }

    async fn start_with(model: MockCompletionModel) -> (Arc<Web>, SocketAddr) {
        let (web, inbox) = Web::new(temp_store(), None, PathBuf::from("unused.json")).unwrap();
        let session = Session::new(model.into(), "", web.clone());
        tokio::spawn(worker(web.clone(), session, inbox));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router(web.clone())).into_future());
        (web, addr)
    }

    /// Sends one request and returns the raw response.
    async fn request(addr: SocketAddr, head: &str, token: Option<&str>, body: &str) -> String {
        let token = token
            .map(|t| format!("x-token: {t}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{head} HTTP/1.1\r\nHost: x\r\n{token}Content-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    fn log(web: &Web) -> Vec<Value> {
        web.inner.lock().unwrap().log.clone()
    }

    /// Waits until some logged event satisfies `matches`.
    async fn wait_for(web: &Web, matches: impl Fn(&Value) -> bool) {
        for _ in 0..500 {
            if log(web).iter().any(&matches) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("event never arrived: {:#?}", log(web));
    }

    #[test]
    fn adjacent_text_is_merged_in_the_log_only() {
        let (web, _inbox) = Web::new(temp_store(), None, PathBuf::from("unused.json")).unwrap();
        let (_, mut live) = web.subscribe();
        web.emit(Event::Text("hel".into()));
        web.emit(Event::Text("lo".into()));
        web.emit(Event::Note("n".into()));
        web.emit(Event::Text("again".into()));
        let texts: Vec<_> = log(&web).iter().map(|e| e["text"].clone()).collect();
        assert_eq!(texts, ["hello", "n", "again"]);
        // Live browsers get the fragments as they come.
        assert_eq!(live.try_recv().unwrap()["text"], "hel");
        assert_eq!(live.try_recv().unwrap()["text"], "lo");
    }

    /// Every update goes to the open pages; a page that connects later gets only the latest.
    #[test]
    fn only_the_latest_context_usage_is_replayed() {
        let (web, _inbox) = Web::new(temp_store(), None, PathBuf::from("unused.json")).unwrap();
        let (_, mut live) = web.subscribe();
        let usage = |tokens| ContextUsage {
            tokens,
            window: Some(200_000),
        };
        web.emit(Event::Context(usage(1_000)));
        web.emit(Event::Text("hi".into()));
        web.emit(Event::Context(usage(42_000)));
        assert_eq!(live.try_recv().unwrap()["tokens"], 1_000);
        assert_eq!(live.try_recv().unwrap()["text"], "hi");
        let latest = live.try_recv().unwrap();
        assert_eq!(
            latest,
            json!({
                "type": "context",
                "tokens": 42_000,
                "window": 200_000,
                "percent": 21,
                "text": "42k / 200k tokens (21%)",
            })
        );
        let (replayed, _) = web.subscribe();
        let types: Vec<&str> = replayed.iter().filter_map(|e| e["type"].as_str()).collect();
        assert_eq!(types, ["session", "tools", "skills", "text", "context"]);
        assert_eq!(replayed.last(), Some(&latest));
        // Without a window the page shows only the tokens.
        web.emit(Event::Context(ContextUsage {
            tokens: 5,
            window: None,
        }));
        let (replayed, _) = web.subscribe();
        let last = replayed.last().unwrap();
        assert_eq!(
            (&last["window"], &last["percent"]),
            (&Value::Null, &Value::Null)
        );
        assert_eq!(last["text"], "5 tokens");
    }

    #[test]
    fn clip_keeps_character_boundaries() {
        assert_eq!(clip("short".into()), "short");
        let long = "é".repeat(MAX_OUTPUT);
        let clipped = clip(long);
        assert!(clipped.starts_with("éé") && clipped.contains("more bytes not shown"));
    }

    #[test]
    fn tool_events_carry_the_full_arguments() {
        // A tool may shorten its title (a script to its first line); the browser
        // still gets all arguments.
        let code = "const a = 1;\nconsole.log(a);";
        let call = ToolCall {
            id: "t1".into(),
            name: "execute_javascript".into(),
            kind: "execute",
            title: "[javascript] const a = 1; ...".into(),
            ask: false,
            args: json!({"code": code, "hosts": ["a.com"]}),
            agent: None,
        };
        let event = tool_event(&call, "running");
        assert_eq!(event["args"]["code"], code);
        assert_eq!(event["args"]["hosts"], json!(["a.com"]));
        let long = ToolCall {
            args: json!({"code": "x".repeat(MAX_OUTPUT + 10)}),
            ..call
        };
        let clipped = tool_event(&long, "running")["args"]["code"].clone();
        assert!(clipped.as_str().unwrap().contains("more bytes not shown"));
    }

    #[test]
    fn tokens_are_compared_exactly() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abd", "abc"));
        assert!(!token_matches("ab", "abc"));
    }

    #[tokio::test]
    async fn api_needs_the_token_but_the_page_does_not() {
        let (web, addr) = start(vec![]).await;
        let page = request(addr, "GET /", None, "").await;
        assert!(
            page.starts_with("HTTP/1.1 200") && page.contains("<title>"),
            "{page}"
        );
        let denied = request(addr, "POST /api/prompt", None, r#"{"text":"hi"}"#).await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        let wrong = request(addr, "POST /api/prompt", Some("nope"), r#"{"text":"hi"}"#).await;
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        let empty = request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"  "}"#).await;
        assert!(empty.starts_with("HTTP/1.1 400"), "{empty}");
        assert!(log(&web).is_empty());
    }

    #[tokio::test]
    async fn pages_fall_back_to_the_index_but_missing_files_do_not() {
        let (_web, addr) = start(vec![]).await;
        let route = request(addr, "GET /settings", None, "").await;
        assert!(route.starts_with("HTTP/1.1 200") && route.contains("<title>"), "{route}");
        let missing = request(addr, "GET /assets/nope.js", None, "").await;
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
        let api = request(addr, "GET /api/sessions", None, "").await;
        assert!(api.starts_with("HTTP/1.1 401"), "{api}");
    }

    /// A server whose settings file is `path`; Anthropic with a key builds without network.
    async fn start_with_config(path: PathBuf) -> (Arc<Web>, SocketAddr) {
        let (web, inbox) = Web::new(temp_store(), None, path).unwrap();
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let session = Session::new(model.into(), "", web.clone());
        tokio::spawn(worker(web.clone(), session, inbox));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router(web.clone())).into_future());
        (web, addr)
    }

    #[tokio::test]
    async fn one_password_is_set_up_first_and_then_logs_in() {
        let dir = std::env::temp_dir().join(format!("agent_web_auth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (_web, addr) = start_with_config(dir.join("config.json")).await;

        let status = request(addr, "GET /api/auth", None, "").await;
        assert!(status.contains(r#""configured":false"#) && status.contains(r#""authenticated":false"#), "{status}");
        assert!(request(addr, "GET /api/config", None, "").await.starts_with("HTTP/1.1 401"));
        let short = request(addr, "POST /api/auth/setup", None, r#"{"password":""}"#).await;
        assert!(short.starts_with("HTTP/1.1 400"), "{short}");

        let created = request(addr, "POST /api/auth/setup", None, r#"{"password":"a"}"#).await;
        assert!(created.starts_with("HTTP/1.1 200"), "{created}");
        let token = created.split(r#""token":""#).nth(1).unwrap().split('"').next().unwrap().to_owned();
        let listed = request(addr, "GET /api/config", Some(&token), "").await;
        assert!(listed.starts_with("HTTP/1.1 200"), "{listed}");
        let status = request(addr, "GET /api/auth", Some(&token), "").await;
        assert!(status.contains(r#""configured":true"#) && status.contains(r#""authenticated":true"#), "{status}");

        // A password can't be set a second time, and a wrong one doesn't log in.
        let again = request(addr, "POST /api/auth/setup", None, r#"{"password":"something else"}"#).await;
        assert!(again.starts_with("HTTP/1.1 409"), "{again}");
        let wrong = request(addr, "POST /api/auth/login", None, r#"{"password":"nope nope nope"}"#).await;
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        let right = request(addr, "POST /api/auth/login", None, r#"{"password":"a"}"#).await;
        assert!(right.starts_with("HTTP/1.1 200") && right.contains(r#""token""#), "{right}");

        let out = request(addr, "POST /api/auth/logout", Some(&token), "").await;
        assert!(out.starts_with("HTTP/1.1 204"), "{out}");
        assert!(request(addr, "GET /api/config", Some(&token), "").await.starts_with("HTTP/1.1 401"));
    }

    #[tokio::test]
    async fn settings_are_read_and_saved_through_the_api() {
        let dir = std::env::temp_dir().join(format!("agent_web_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let (web, addr) = start_with_config(path.clone()).await;
        let token = Some(web.token.as_str());

        let denied = request(addr, "GET /api/config", None, "").await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        let listed = request(addr, "GET /api/config", token, "").await;
        assert!(listed.starts_with("HTTP/1.1 200") && listed.contains("LLM_BACKEND"), "{listed}");

        let body = r#"{"LLM_BACKEND":"anthropic","ANTHROPIC_API_KEY":"sk-ant-secret","LLM_MODEL":"m"}"#;
        let saved = request(addr, "PUT /api/config", token, body).await;
        assert!(saved.starts_with("HTTP/1.1 200"), "{saved}");
        assert!(!saved.contains("sk-ant-secret"), "{saved}");
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(file.contains("sk-ant-secret") && file.contains("anthropic"), "{file}");

        let bad = request(addr, "PUT /api/config", token, r#"{"LLM_BACKEND":"gemini"}"#).await;
        assert!(bad.starts_with("HTTP/1.1 400") && bad.contains("gemini"), "{bad}");
        assert!(!std::fs::read_to_string(&path).unwrap().contains("gemini"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tools_are_chosen_per_conversation_and_remembered_with_it() {
        let (web, addr) = start(vec![turn(MockStreamEvent::text("hi"))]).await;
        let token = Some(web.token.as_str());
        let enabled = |web: &Web| Web::tools_event(&web.inner.lock().unwrap())["enabled"].clone();
        assert!(enabled(&web).as_array().unwrap().len() > 2);

        let bad = request(addr, "POST /api/tools", token, r#"{"tools":["nope"]}"#).await;
        assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
        let ok = request(addr, "POST /api/tools", token, r#"{"tools":["execute"]}"#).await;
        assert!(ok.starts_with("HTTP/1.1 202"), "{ok}");
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        assert_eq!(enabled(&web), json!(["execute"]));

        // The choice is saved with the conversation, and a new one starts without it.
        request(addr, "POST /api/prompt", token, r#"{"text":"hello"}"#).await;
        wait_for(&web, |e| e["type"] == "text").await;
        let id = web.inner.lock().unwrap().session_id.clone().unwrap();
        assert_eq!(web.store.load(&id).unwrap().choices.tools, Some(vec!["execute".to_owned()]));
        request(addr, "POST /api/session", token, "{}").await;
        for _ in 0..100 {
            if web.inner.lock().unwrap().choices.tools.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(enabled(&web).as_array().unwrap().len() > 2);
    }

    #[tokio::test]
    async fn the_file_api_is_confined_and_skills_changes_refresh_the_agent() {
        let (web, addr) = start(vec![]).await;
        let token = Some(web.token.as_str());
        let dir = format!("test-files-{}", std::process::id());
        let post = |path: &'static str, body: String| async move {
            request(addr, &format!("POST /api/files/{path}"), token, &body).await
        };

        let denied = request(addr, "POST /api/files/list", None, r#"{"root":"work"}"#).await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        let unknown = post("list", r#"{"root":"src","path":""}"#.into()).await;
        assert!(unknown.starts_with("HTTP/1.1 400"), "{unknown}");
        let escape = post("write", r#"{"root":"work","path":"../x.txt","content":"x"}"#.into()).await;
        assert!(escape.starts_with("HTTP/1.1 400"), "{escape}");

        let wrote = post("write", format!(r#"{{"root":"work","path":"{dir}/a.md","content":"hi"}}"#)).await;
        assert!(wrote.starts_with("HTTP/1.1 200"), "{wrote}");
        let read = post("read", format!(r#"{{"root":"work","path":"{dir}/a.md"}}"#)).await;
        assert!(read.contains(r#""content":"hi""#), "{read}");
        let listed = post("list", format!(r#"{{"root":"work","path":"{dir}"}}"#)).await;
        assert!(listed.contains("a.md"), "{listed}");
        post("delete", format!(r#"{{"root":"work","path":"{dir}"}}"#)).await;
        assert!(!crate::tools::workdir().join(&dir).exists());

        // Work files don't touch the skill list; changes to skills do, and built-in ones are refused.
        assert!(!web.skills_dirty.load(Ordering::SeqCst));
        let builtin = post("write", r#"{"root":"skills","path":"creating-skills/SKILL.md","content":"x"}"#.into()).await;
        assert!(builtin.starts_with("HTTP/1.1 403"), "{builtin}");
        assert!(!web.skills_dirty.load(Ordering::SeqCst));
        let name = format!("zz-test-{}", std::process::id());
        let skill = post("write", format!(r#"{{"root":"skills","path":"{name}/SKILL.md","content":"x"}}"#)).await;
        assert!(skill.starts_with("HTTP/1.1 200"), "{skill}");
        assert!(web.skills_dirty.load(Ordering::SeqCst));
        post("delete", format!(r#"{{"root":"skills","path":"{name}"}}"#)).await;
    }

    /// A server with a worker whose model answers with `turns`; its settings (and hooks) live
    /// in a fresh directory.
    async fn start_hooks(turns: Vec<Vec<MockStreamEvent>>) -> (Arc<Web>, SocketAddr) {
        let dir = std::env::temp_dir().join(format!("agent_web_hooks_{}_{}", std::process::id(), temp_store_n()));
        std::fs::create_dir_all(&dir).unwrap();
        let (web, inbox) = Web::new(temp_store(), None, dir.join("config.json")).unwrap();
        let model = MockCompletionModel::from_stream_turns(turns);
        let session = Session::new(model.into(), "", web.clone());
        tokio::spawn(worker(web.clone(), session, inbox));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router(web.clone())).into_future());
        (web, addr)
    }

    fn temp_store_n() -> usize {
        use std::sync::atomic::AtomicUsize;
        static N: AtomicUsize = AtomicUsize::new(0);
        N.fetch_add(1, Ordering::SeqCst)
    }

    /// Creates a hook through the API and returns its token.
    async fn create_hook(web: &Web, addr: SocketAddr, fields: &str) -> String {
        let response = request(addr, "POST /api/hooks/save", Some(&web.token), fields).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let body = response.split("\r\n\r\n").nth(1).unwrap();
        let value: Value = serde_json::from_str(body.trim()).unwrap();
        value["token"].as_str().unwrap().to_owned()
    }

    /// POSTs to a hook with the token in a header; returns the response.
    async fn trigger(addr: SocketAddr, name: &str, token: &str, body: &str) -> String {
        let head = format!(
            "POST /hooks/{name} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nX-Event: issues\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    fn session_of(response: &str) -> String {
        let body = response.split("\r\n\r\n").nth(1).unwrap();
        let value: Value = serde_json::from_str(body.trim()).unwrap();
        value["session"].as_str().unwrap().to_owned()
    }

    /// The saved session once its run has produced an answer.
    async fn finished(web: &Web, id: &str) -> sessions::Saved {
        for _ in 0..250 {
            if let Ok(saved) = web.store.load(id)
                && saved.log.iter().any(|e| e["type"] == "text")
                && !saved.log.iter().any(|e| e["type"] == "note" && e["text"] == "running")
            {
                return saved;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the run did not finish");
    }

    #[tokio::test]
    async fn a_hook_runs_the_agent_in_a_session_of_its_own() {
        let (web, addr) = start_hooks(vec![turn(MockStreamEvent::text("handled"))]).await;
        let token = create_hook(
            &web,
            addr,
            r#"{"create":true,"name":"gh","template":"Issue: {{body.issue.title}} ({{headers.x-event}}) {{headers.authorization}}","tools":["use_skill"]}"#,
        )
        .await;

        // The hook's own token is needed, and the page's is not enough.
        let none = trigger(addr, "gh", "wrong", "{}").await;
        assert!(none.starts_with("HTTP/1.1 401"), "{none}");
        let ui = trigger(addr, "gh", &web.token, "{}").await;
        assert!(ui.starts_with("HTTP/1.1 401"), "{ui}");
        let unknown = trigger(addr, "nope", &token, "{}").await;
        assert!(unknown.starts_with("HTTP/1.1 401"), "{unknown}");

        let response = trigger(addr, "gh", &token, r#"{"issue":{"title":"Crash"}}"#).await;
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
        let saved = finished(&web, &session_of(&response)).await;
        let prompt = saved.log.iter().find(|e| e["type"] == "user").unwrap()["text"].as_str().unwrap();
        // The token is not offered to the template.
        assert_eq!(prompt, "Issue: Crash (issues) ");
        assert!(saved.log.iter().any(|e| e["type"] == "text" && e["text"] == "handled"));
        assert_eq!(saved.choices.tools, Some(vec!["use_skill".to_owned()]));
        assert_eq!(saved.history.len(), 2);
        // It is listed under the hook's name, and the open chat was not touched.
        let listed = web.store.list();
        assert_eq!(listed[0]["title"], "hook: gh");
        assert!(log(&web).is_empty());

        // A switched-off hook refuses.
        request(addr, "POST /api/hooks/save", Some(&web.token), r#"{"name":"gh","template":"x","enabled":false}"#).await;
        let off = trigger(addr, "gh", &token, "{}").await;
        assert!(off.starts_with("HTTP/1.1 403"), "{off}");
    }

    #[cfg(feature = "javascript")]
    /// A hook `w` with the given write mode whose model first says something and then calls
    /// `execute_javascript` with `writes: true`; `later` are the model's following turns.
    async fn start_writer(mode: &str, later: Vec<Vec<MockStreamEvent>>) -> (Arc<Web>, SocketAddr, String) {
        let call = json!({"code": "console.log('wrote')", "writes": true});
        let mut turns = vec![vec![
            MockStreamEvent::text("I will write"),
            MockStreamEvent::tool_call("t1", "execute_javascript", call),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]];
        turns.extend(later);
        let (web, addr) = start_hooks(turns).await;
        let fields = format!(r#"{{"create":true,"name":"w","template":"go","tools":["execute_javascript"],"writes":"{mode}"}}"#);
        let token = create_hook(&web, addr, &fields).await;
        let id = session_of(&trigger(addr, "w", &token, "{}").await);
        (web, addr, id)
    }

    #[cfg(feature = "javascript")]
    /// The saved session once `done` holds for it.
    async fn wait_saved(web: &Web, id: &str, done: impl Fn(&sessions::Saved) -> bool) -> sessions::Saved {
        for _ in 0..250 {
            if let Ok(saved) = web.store.load(id)
                && done(&saved)
            {
                return saved;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the session did not get there");
    }

    #[cfg(feature = "javascript")]
    async fn decide(addr: SocketAddr, web: &Web, body: &str) -> String {
        request(addr, "POST /api/approvals/decide", Some(&web.token), body).await
    }

    #[cfg(feature = "javascript")]
    fn history_text(saved: &sessions::Saved) -> String {
        format!("{:?}", saved.history)
    }

    #[cfg(feature = "javascript")]
    #[tokio::test]
    async fn a_write_pauses_the_run_until_the_user_approves() {
        let (web, addr, id) = start_writer("approve", vec![turn(MockStreamEvent::text("all fine"))]).await;
        let waiting = wait_saved(&web, &id, |s| s.pending.is_some()).await;
        let pending = waiting.pending.clone().unwrap();
        assert_eq!((pending["hook"].as_str(), pending["message"].as_str()), (Some("w"), Some("I will write")));
        assert_eq!(pending["call"]["args"]["code"], "console.log('wrote')");
        // The model saw no refusal: the history holds a digest, and the call shows as waiting.
        assert!(history_text(&waiting).contains("Paused: waiting for the user's approval"), "{:?}", waiting.history);
        assert!(waiting.log.iter().any(|e| e["id"] == "t1" && e["status"] == "waiting"), "{:?}", waiting.log);
        assert!(!waiting.log.iter().any(|e| e["type"] == "text" && e["text"] == "all fine"));
        assert_eq!(web.store.list()[0]["waiting"], true);

        let listed = request(addr, "GET /api/approvals", Some(&web.token), "").await;
        assert!(listed.contains(&id) && listed.contains("I will write") && listed.contains("console.log('wrote')"), "{listed}");
        assert!(request(addr, "GET /api/approvals", None, "").await.starts_with("HTTP/1.1 401"));

        let response = decide(addr, &web, &format!(r#"{{"session":"{id}","approve":true}}"#)).await;
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
        let done = wait_saved(&web, &id, |s| s.log.iter().any(|e| e["type"] == "text" && e["text"] == "all fine")).await;
        // The call ran with its output, which the model got back; nothing waits any more.
        assert!(done.pending.is_none());
        let tool = done.log.iter().rev().find(|e| e["id"] == "t1").unwrap();
        assert_eq!(tool["status"], "done");
        assert!(tool["output"].as_str().unwrap().contains("wrote"), "{tool}");
        let history = history_text(&done);
        assert!(history.contains("The user approved your execute_javascript call and it ran. Output:"), "{history}");
        assert!(history.contains(r"Output:\nok\nstdout:\nwrote"), "{history}");
        // Deciding again is too late.
        let again = decide(addr, &web, &format!(r#"{{"session":"{id}","approve":true}}"#)).await;
        assert!(again.starts_with("HTTP/1.1 409"), "{again}");
        assert!(request(addr, "GET /api/approvals", Some(&web.token), "").await.contains(r#""approvals":[]"#));
    }

    #[tokio::test]
    async fn a_build_from_source_shows_its_version_and_refuses_to_update() {
        let (web, addr) = start(vec![]).await;
        assert!(request(addr, "GET /api/version", None, "").await.starts_with("HTTP/1.1 401"));
        let shown = request(addr, "GET /api/version", Some(&web.token), "").await;
        assert!(shown.contains(&format!(r#""version":"{}""#, update::VERSION)), "{shown}");
        assert!(shown.contains(r#""can_update":false"#) && shown.contains(r#""dev":true"#), "{shown}");
        let refused = request(addr, "POST /api/update", Some(&web.token), "{}").await;
        assert!(refused.starts_with("HTTP/1.1 409"), "{refused}");
        assert!(request(addr, "POST /api/update", None, "{}").await.starts_with("HTTP/1.1 401"));
    }

    #[cfg(feature = "javascript")]
    #[tokio::test]
    async fn a_declined_write_does_not_run_and_the_comment_reaches_the_model() {
        let (web, addr, id) = start_writer("approve", vec![turn(MockStreamEvent::text("understood"))]).await;
        wait_saved(&web, &id, |s| s.pending.is_some()).await;
        let body = format!(r#"{{"session":"{id}","approve":false,"comment":"not on weekends"}}"#);
        assert!(decide(addr, &web, &body).await.starts_with("HTTP/1.1 202"));
        let done = wait_saved(&web, &id, |s| s.log.iter().any(|e| e["type"] == "text" && e["text"] == "understood")).await;
        let history = history_text(&done);
        assert!(history.contains("The user declined your execute_javascript call."), "{history}");
        assert!(history.contains("Their comment: not on weekends"), "{history}");
        assert!(!history.contains("Output:"), "{history}");
        assert!(done.log.iter().any(|e| e["id"] == "t1" && e["status"] == "denied"), "{:?}", done.log);
    }

    #[cfg(feature = "javascript")]
    #[tokio::test]
    async fn decisions_need_a_waiting_session_and_a_usable_hook() {
        let (web, addr, id) = start_writer("approve", vec![turn(MockStreamEvent::text("ok"))]).await;
        wait_saved(&web, &id, |s| s.pending.is_some()).await;
        let status = |response: String| response.lines().next().unwrap().to_owned();
        assert!(status(decide(addr, &web, r#"{"session":"nope","approve":true}"#).await).contains("404"));
        assert!(status(decide(addr, &web, r#"{"session":"../x","approve":true}"#).await).contains("400"));
        assert!(status(decide(addr, &web, &format!(r#"{{"session":"{id}"}}"#)).await).contains("400"));
        // A session that never waited.
        let plain = web.store.new_id();
        web.store.save(&plain, &[], &[], None, &Choices::default(), None).unwrap();
        let response = decide(addr, &web, &format!(r#"{{"session":"{plain}","approve":true}}"#)).await;
        assert!(response.starts_with("HTTP/1.1 409") && response.contains("not waiting"), "{response}");
        // Its hook switched off, then deleted.
        request(addr, "POST /api/hooks/save", Some(&web.token), r#"{"name":"w","template":"go","enabled":false}"#).await;
        let response = decide(addr, &web, &format!(r#"{{"session":"{id}","approve":true}}"#)).await;
        assert!(response.starts_with("HTTP/1.1 409") && response.contains("switched off"), "{response}");
        request(addr, "POST /api/hooks/delete", Some(&web.token), r#"{"name":"w"}"#).await;
        let response = decide(addr, &web, &format!(r#"{{"session":"{id}","approve":false}}"#)).await;
        assert!(response.starts_with("HTTP/1.1 409") && response.contains("deleted"), "{response}");
        // Nothing was decided: it still waits.
        assert!(web.store.load(&id).unwrap().pending.is_some());
    }

    #[cfg(feature = "javascript")]
    #[tokio::test]
    async fn the_write_modes_deny_and_allow_never_wait() {
        // deny: the model is told the call was declined and the run ends by itself.
        let (web, _addr, id) = start_writer("deny", vec![turn(MockStreamEvent::text("gave up"))]).await;
        let done = wait_saved(&web, &id, |s| s.log.iter().any(|e| e["type"] == "text" && e["text"] == "gave up")).await;
        assert!(done.pending.is_none() && history_text(&done).contains("the user denied this tool call"), "{:?}", done.history);
        // allow: the call runs at once.
        let (web, _addr, id) = start_writer("allow", vec![turn(MockStreamEvent::text("written"))]).await;
        let done = wait_saved(&web, &id, |s| s.log.iter().any(|e| e["type"] == "text" && e["text"] == "written")).await;
        assert!(done.pending.is_none() && history_text(&done).contains(r"ok\nstdout:\nwrote"), "{:?}", done.history);
    }

    #[tokio::test]
    async fn runs_go_one_after_the_other_and_a_failed_tool_is_reported_to_the_model() {
        // `execute` is not among the hook's tools, so the model is told it doesn't exist.
        let (web, addr) = start_hooks(vec![
            turn(MockStreamEvent::tool_call("t1", "execute", json!({"command": "ls"}))),
            turn(MockStreamEvent::text("first done")),
            turn(MockStreamEvent::text("second done")),
        ])
        .await;
        let token = create_hook(&web, addr, r#"{"create":true,"name":"q","template":"go {{body.n}}"}"#).await;
        let a = trigger(addr, "q", &token, r#"{"n":1}"#).await;
        let b = trigger(addr, "q", &token, r#"{"n":2}"#).await;
        let (first, second) = (session_of(&a), session_of(&b));
        assert_ne!(first, second);
        let one = finished(&web, &first).await;
        let two = finished(&web, &second).await;
        assert!(one.log.iter().any(|e| e["type"] == "text" && e["text"] == "first done"), "{:?}", one.log);
        assert!(two.log.iter().any(|e| e["type"] == "text" && e["text"] == "second done"));
        assert!(one.log.iter().any(|e| e["type"] == "note"), "the unknown tool is noted: {:?}", one.log);
    }

    #[tokio::test]
    async fn the_result_goes_to_the_callback_url() {
        let callback = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/cb", callback.local_addr().unwrap());
        let received = tokio::spawn(async move {
            let (mut stream, _) = callback.accept().await.unwrap();
            let mut buffer = vec![0u8; 8192];
            let n = stream.read(&mut buffer).await.unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            String::from_utf8_lossy(&buffer[..n]).into_owned()
        });
        let (web, addr) = start_hooks(vec![turn(MockStreamEvent::text("the result"))]).await;
        let token = create_hook(
            &web,
            addr,
            &format!(r#"{{"create":true,"name":"cb","template":"x","callback_url":"{url}"}}"#),
        )
        .await;
        let response = trigger(addr, "cb", &token, "{}").await;
        let id = session_of(&response);
        let request = tokio::time::timeout(Duration::from_secs(5), received).await.unwrap().unwrap();
        assert!(request.starts_with("POST /cb"), "{request}");
        assert!(request.contains(r#""status":"done""#) && request.contains("the result"), "{request}");
        assert!(request.contains(&id), "{request}");
    }

    #[tokio::test]
    async fn too_many_waiting_runs_are_refused() {
        // No worker: nothing is taken from the queue.
        let dir = std::env::temp_dir().join(format!("agent_web_limit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (web, _inbox) = Web::new(temp_store(), None, dir.join("config.json")).unwrap();
        let fields = Fields::parse(&json!({"name": "f", "template": "t"}), false).unwrap();
        let (hook, _) = web.hooks.lock().unwrap().upsert(fields, || "tok".into()).unwrap();
        for n in 1..=hooks::MAX_QUEUE {
            assert_eq!(enqueue(&web, hook.clone(), "p".into()).unwrap().1, n);
        }
        assert_eq!(enqueue(&web, hook, "p".into()).unwrap_err(), StatusCode::TOO_MANY_REQUESTS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn hook_settings_are_validated_and_tokens_never_listed() {
        let (web, addr) = start_hooks(vec![]).await;
        let token = Some(web.token.as_str());
        let denied = request(addr, "GET /api/hooks", None, "").await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        for bad in [
            r#"{"create":true,"name":"a","template":"x","tools":["ask_user"]}"#,
            r#"{"create":true,"name":"a","template":"x","tools":["execute"]}"#,
            r#"{"create":true,"name":"A b","template":"x"}"#,
            r#"{"create":true,"name":"a","template":""}"#,
        ] {
            let response = request(addr, "POST /api/hooks/save", token, bad).await;
            assert!(response.starts_with("HTTP/1.1 400"), "{bad}: {response}");
        }
        let secret = create_hook(&web, addr, r#"{"create":true,"name":"a","template":"x"}"#).await;
        let again = request(addr, "POST /api/hooks/save", token, r#"{"create":true,"name":"a","template":"x"}"#).await;
        assert!(again.starts_with("HTTP/1.1 409"), "{again}");
        let listed = request(addr, "GET /api/hooks", token, "").await;
        assert!(listed.contains(r#""name":"a""#) && !listed.contains(&secret), "{listed}");
        // A new token replaces the old one.
        let renewed = request(addr, "POST /api/hooks/token", token, r#"{"name":"a"}"#).await;
        assert!(renewed.starts_with("HTTP/1.1 200") && !renewed.contains(&secret), "{renewed}");
        let old = trigger(addr, "a", &secret, "{}").await;
        assert!(old.starts_with("HTTP/1.1 401"), "{old}");
        let gone = request(addr, "POST /api/hooks/delete", token, r#"{"name":"a"}"#).await;
        assert!(gone.starts_with("HTTP/1.1 200"), "{gone}");
    }

    #[tokio::test]
    async fn skills_are_switched_per_conversation_and_remembered_with_it() {
        let (web, addr) = start(vec![turn(MockStreamEvent::text("hi"))]).await;
        let token = Some(web.token.as_str());
        let disabled = |web: &Web| Web::skills_event(&web.inner.lock().unwrap())["disabled"].clone();
        assert_eq!(disabled(&web), json!([]));

        let bad = request(addr, "POST /api/skills", token, r#"{"disabled":[1]}"#).await;
        assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
        let ok = request(addr, "POST /api/skills", token, r#"{"disabled":["paperless"]}"#).await;
        assert!(ok.starts_with("HTTP/1.1 202"), "{ok}");
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        assert_eq!(disabled(&web), json!(["paperless"]));
        assert_eq!(Web::skills_event(&web.inner.lock().unwrap())["custom"], true);

        // Saved with the conversation; a new one goes back to the default.
        request(addr, "POST /api/prompt", token, r#"{"text":"hello"}"#).await;
        wait_for(&web, |e| e["type"] == "text").await;
        let id = web.inner.lock().unwrap().session_id.clone().unwrap();
        assert_eq!(
            web.store.load(&id).unwrap().choices.skills_disabled,
            Some(vec!["paperless".to_owned()])
        );
        request(addr, "POST /api/session", token, "{}").await;
        for _ in 0..100 {
            if web.inner.lock().unwrap().choices.skills_disabled.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(disabled(&web), json!([]));
    }

    /// The slider only exists for models that list levels, and only those can be picked.
    #[tokio::test]
    async fn thinking_level_is_chosen_from_the_models_levels() {
        let thinking = Thinking {
            levels: vec!["off".into(), "low".into(), "xhigh".into()],
            default: "low".into(),
        };
        let (web, _inbox) = Web::new(temp_store(), Some(thinking), PathBuf::from("unused.json")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router(web.clone())).into_future());

        let (events, _) = web.subscribe();
        assert_eq!(events[1]["type"], "thinking");
        assert_eq!(events[1]["current"], "low");
        assert_eq!(
            web.think_param(),
            None,
            "nothing is sent until the user picks"
        );

        let unknown = request(
            addr,
            "POST /api/thinking",
            Some(&web.token),
            r#"{"level":"max"}"#,
        )
        .await;
        assert!(unknown.starts_with("HTTP/1.1 400"), "{unknown}");
        let ok = request(
            addr,
            "POST /api/thinking",
            Some(&web.token),
            r#"{"level":"xhigh"}"#,
        )
        .await;
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert_eq!(web.think_param(), Some(json!("xhigh")));
        let off = request(
            addr,
            "POST /api/thinking",
            Some(&web.token),
            r#"{"level":"off"}"#,
        )
        .await;
        assert!(off.starts_with("HTTP/1.1 204"), "{off}");
        assert_eq!(web.think_param(), Some(json!(false)));
        assert_eq!(web.subscribe().0[1]["current"], "off");
    }

    #[tokio::test]
    async fn no_levels_no_thinking_endpoint() {
        let (web, addr) = start(vec![]).await;
        assert!(web.subscribe().0.iter().all(|e| e["type"] != "thinking"));
        let denied = request(
            addr,
            "POST /api/thinking",
            Some(&web.token),
            r#"{"level":"low"}"#,
        )
        .await;
        assert!(denied.starts_with("HTTP/1.1 404"), "{denied}");
    }

    /// A browser that connects late gets the transcript first, then live events.
    #[tokio::test]
    async fn event_stream_replays_then_goes_live() {
        let (web, addr) = start(vec![]).await;
        web.emit(Event::Text("before".into()));
        let head = format!(
            "GET /api/events?token={} HTTP/1.1\r\nHost: x\r\n\r\n",
            web.token
        );
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut seen = String::new();
        let mut buffer = [0u8; 4096];
        for expected in ["before", "after"] {
            if expected == "after" {
                web.emit(Event::Text("after".into()));
            }
            while !seen.contains(&format!("\"text\":\"{expected}\"")) {
                let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
                    .await
                    .expect("no event")
                    .unwrap();
                assert!(read > 0, "stream closed: {seen}");
                seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
            }
        }
        assert!(seen.contains("text/event-stream"), "{seen}");
        assert!(seen.find("before") < seen.find("after"));
    }

    #[tokio::test]
    async fn prompt_streams_the_answer() {
        let (web, addr) = start(vec![turn(MockStreamEvent::text("hello"))]).await;
        let response = request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"hi"}"#).await;
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "hello").await;
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        assert_eq!(log(&web)[0], json!({"type": "user", "text": "hi"}));
    }

    /// A call that needs approval waits for the browser's answer, then runs.
    /// `ask_user` waits for the browser's answer, which becomes the tool result.
    #[tokio::test]
    async fn question_is_asked_and_answered() {
        let args = json!({"question": "Which color?", "options": ["red", "blue"]});
        let model = MockCompletionModel::from_stream_turns([
            turn(MockStreamEvent::tool_call("t1", "ask_user", args)),
            turn(MockStreamEvent::text("done")),
        ]);
        let recorded = model.clone();
        let (web, addr) = start_with(model).await;
        request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"go"}"#).await;
        wait_for(&web, |e| {
            e["type"] == "question" && e["status"] == "pending"
        })
        .await;
        let body = r#"{"id":"q0","answer":"blue"}"#;
        let ok = request(addr, "POST /api/answer", Some(&web.token), body).await;
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        let again = request(addr, "POST /api/answer", Some(&web.token), body).await;
        assert!(again.starts_with("HTTP/1.1 404"), "{again}");
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "done").await;
        let followup = format!("{:?}", recorded.requests()[1].chat_history);
        assert!(followup.contains("The user answered: blue"), "{followup}");
    }

    #[tokio::test]
    async fn approval_is_asked_and_the_tool_runs() {
        let args = json!({"command": "echo web-approved"});
        let (web, addr) = start(vec![
            turn(MockStreamEvent::tool_call("t1", "execute", args)),
            turn(MockStreamEvent::text("done")),
        ])
        .await;
        request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"run"}"#).await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "pending").await;
        let busy = request(
            addr,
            "POST /api/prompt",
            Some(&web.token),
            r#"{"text":"again"}"#,
        )
        .await;
        assert!(busy.starts_with("HTTP/1.1 409"), "{busy}");
        let body = r#"{"id":"t1","allow":true}"#;
        let answered = request(addr, "POST /api/approve", Some(&web.token), body).await;
        assert!(answered.starts_with("HTTP/1.1 204"), "{answered}");
        let unknown = request(addr, "POST /api/approve", Some(&web.token), body).await;
        assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
        wait_for(&web, |e| {
            e["id"] == "t1"
                && e["status"] == "done"
                && e["output"]
                    .as_str()
                    .is_some_and(|o| o.contains("web-approved"))
        })
        .await;
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "done").await;
    }

    /// A subagent's text and tool calls are tagged with its id, so the page can show
    /// them in a panel of their own; its tool call ids can't clash with the main agent's.
    #[tokio::test]
    async fn subagent_events_carry_the_agent_id() {
        let task = json!({"description": "say hi", "prompt": "Say hi."});
        let run = json!({"command": "echo from-sub"});
        let (web, addr) = start(vec![
            turn(MockStreamEvent::tool_call("t1", "agent", task)),
            turn(MockStreamEvent::tool_call("t1", "execute", run)),
            turn(MockStreamEvent::text("report")),
            turn(MockStreamEvent::text("done")),
        ])
        .await;
        request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"go"}"#).await;
        wait_for(&web, |e| {
            e["type"] == "subagent" && e["status"] == "running"
        })
        .await;
        wait_for(&web, |e| e["type"] == "tool" && e["status"] == "pending").await;
        let call = log(&web)
            .into_iter()
            .find(|e| e["status"] == "pending")
            .unwrap();
        let (id, agent) = (
            call["id"].as_str().unwrap(),
            call["agent"].as_str().unwrap(),
        );
        assert_eq!(id, format!("{agent}/t1"));
        let body = json!({"id": id, "allow": true}).to_string();
        request(addr, "POST /api/approve", Some(&web.token), &body).await;
        wait_for(&web, |e| e["id"] == id && e["status"] == "done").await;
        wait_for(&web, |e| {
            e["type"] == "text" && e["agent"] == agent && e["text"] == "report"
        })
        .await;
        wait_for(&web, |e| e["type"] == "subagent" && e["status"] == "done").await;
        wait_for(&web, |e| {
            e["type"] == "text" && e["agent"].is_null() && e["text"] == "done"
        })
        .await;
        let started = log(&web)
            .into_iter()
            .find(|e| e["type"] == "subagent")
            .unwrap();
        assert_eq!(started["description"], "say hi");
        assert_eq!(started["agent"], agent);
    }

    #[tokio::test]
    async fn denied_calls_do_not_run() {
        let args = json!({"command": "echo never"});
        let (web, addr) = start(vec![
            turn(MockStreamEvent::tool_call("t1", "execute", args)),
            turn(MockStreamEvent::text("ok")),
        ])
        .await;
        request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"run"}"#).await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "pending").await;
        request(
            addr,
            "POST /api/approve",
            Some(&web.token),
            r#"{"id":"t1","allow":false}"#,
        )
        .await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "denied").await;
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "ok").await;
        assert!(!log(&web).iter().any(|e| e["status"] == "done"));
    }

    /// Stopping while a call waits for approval ends the prompt and frees the session.
    #[tokio::test]
    async fn cancel_ends_a_prompt_waiting_for_approval() {
        let args = json!({"command": "echo never"});
        let (web, addr) = start(vec![
            turn(MockStreamEvent::tool_call("t1", "execute", args)),
            turn(MockStreamEvent::text("second")),
        ])
        .await;
        request(addr, "POST /api/prompt", Some(&web.token), r#"{"text":"run"}"#).await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "pending").await;
        request(addr, "POST /api/cancel", Some(&web.token), "").await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "cancelled").await;
        wait_for(&web, |e| e["type"] == "note" && e["text"] == "stopped").await;
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        assert!(!web.busy.load(Ordering::SeqCst));
    }

    /// A prompt is saved; another page can list, open, and delete the session.
    #[tokio::test]
    async fn sessions_are_saved_listed_opened_and_deleted() {
        let (web, addr) = start(vec![
            turn(MockStreamEvent::text("one")),
            turn(MockStreamEvent::text("two")),
        ])
        .await;
        let token = Some(web.token.as_str());
        request(addr, "POST /api/prompt", token, r#"{"text":"first chat"}"#).await;
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        let listed = request(addr, "GET /api/sessions", token, "").await;
        assert!(listed.contains("first chat"), "{listed}");
        let id = web.inner.lock().unwrap().session_id.clone().unwrap();
        assert!(web.store.dir().join(format!("{id}.json")).exists());

        // A new session starts empty ...
        let started = request(addr, "POST /api/session", token, "{}").await;
        assert!(started.starts_with("HTTP/1.1 202"), "{started}");
        for _ in 0..500 {
            if log(&web).iter().all(|e| e["type"] == "status") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(web.inner.lock().unwrap().session_id.is_none());
        request(addr, "POST /api/prompt", token, r#"{"text":"second chat"}"#).await;
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "two").await;
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        assert_eq!(web.store.list().len(), 2);

        // ... and the first one can be opened again with its transcript.
        let body = format!(r#"{{"id":"{id}"}}"#);
        request(addr, "POST /api/session", token, &body).await;
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "one").await;
        assert!(!log(&web).iter().any(|e| e["text"] == "two"));
        let bad = request(addr, "POST /api/session", token, r#"{"id":"../x"}"#).await;
        assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");

        request(addr, "POST /api/session/delete", token, &body).await;
        for _ in 0..500 {
            if web.store.list().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(web.store.list().len(), 1);
        let _ = std::fs::remove_dir_all(web.store.dir());
    }
}
