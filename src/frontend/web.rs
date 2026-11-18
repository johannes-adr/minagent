//! Web frontend: `agent --web [addr]` serves a chat page (default `127.0.0.1:8080`).
//!
//! The page ([`web.html`](web.html)) is one self-contained file. It gets the session as a
//! stream of JSON events over server-sent events (`GET /events`) and talks back with
//! `POST /prompt`, `POST /approve`, `POST /answer` and `POST /cancel`. Like the terminal UI there is one
//! session; every browser tab shows it, and a reloaded tab gets the whole transcript replayed.
//!
//! Whoever can call the API can make the agent run commands, so all of it needs the random
//! token printed at startup (header `x-token`, or `?token=` for the event stream). Only the
//! static page is served without it.

use crate::frontend::{ContextUsage, Event, Frontend, Question, ToolCall};
use crate::harness::Session;
use crate::sessions::{self, Store};
use crate::thinking::{self, Thinking};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use rig_core::serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast, mpsc, oneshot};

/// The receiving end of what the browser asks for, see [`Web::new`].
pub type Inbox = mpsc::UnboundedReceiver<Command>;

const PAGE: &str = include_str!("web.html");
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
    /// The thinking level the user picked; `None` leaves it to the model's default.
    think: Option<String>,
}

/// What the worker does, one at a time.
pub enum Command {
    Prompt(String),
    /// Shows a stored session, or a new one for `None`.
    Switch(Option<String>),
    Delete(String),
}

/// The web frontend's state, shared by the harness side ([`Frontend`]) and the HTTP handlers.
pub struct Web {
    token: String,
    inner: Mutex<Inner>,
    live: broadcast::Sender<Value>,
    busy: AtomicBool,
    commands: mpsc::UnboundedSender<Command>,
    store: Store,
    cancel: Notify,
    /// The thinking levels the model offers, if it offers any.
    thinking: Option<Thinking>,
}

impl Web {
    /// Returns the frontend and the receiving end of the prompts the browser submits,
    /// which [`run`] feeds to the session.
    pub fn new(store: Store, thinking: Option<Thinking>) -> io::Result<(Arc<Self>, Inbox)> {
        let (commands, inbox) = mpsc::unbounded_channel();
        let web = Self {
            token: random_token()?,
            inner: Mutex::default(),
            live: broadcast::channel(4096).0,
            busy: AtomicBool::new(false),
            commands,
            store,
            cancel: Notify::new(),
            thinking,
        };
        Ok((Arc::new(web), inbox))
    }

    /// Records `event` and sends it to the connected browsers.
    fn push(&self, event: Value) {
        let mut inner = self.inner.lock().unwrap();
        if matches!(event["type"].as_str(), Some("text" | "reasoning"))
            && let Some(last) = inner.log.last_mut()
            && last["type"] == event["type"]
            && last["agent"] == event["agent"]
            && let Some(Value::String(text)) = last.get_mut("text")
        {
            text.push_str(event["text"].as_str().unwrap_or_default());
        } else {
            inner.log.push(event.clone());
        }
        // Sent under the lock so that `subscribe` sees each event exactly once.
        let _ = self.live.send(event);
    }

    /// Sends the context usage to the connected browsers and keeps it for later ones.
    fn set_context(&self, usage: ContextUsage) {
        let event = json!({
            "type": "context",
            "tokens": usage.tokens,
            "window": usage.window,
            "percent": usage.percent(),
            "text": usage.to_string(),
        });
        let mut inner = self.inner.lock().unwrap();
        inner.context = Some(event.clone());
        let _ = self.live.send(event);
    }

    /// The thinking slider's state, if the model has levels.
    fn thinking_event(&self, inner: &Inner) -> Option<Value> {
        let thinking = self.thinking.as_ref()?;
        Some(json!({
            "type": "thinking",
            "levels": thinking.levels,
            "default": thinking.default,
            "current": inner.think.as_deref().unwrap_or(&thinking.default),
        }))
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
        events.extend(inner.log.clone());
        events.extend(inner.context.clone());
        (events, self.live.subscribe())
    }

    /// Shows another session: the open pages clear their transcript and get this one.
    fn replace(&self, id: Option<String>, log: Vec<Value>, context: Option<Value>) {
        let mut inner = self.inner.lock().unwrap();
        inner.session_id = id;
        inner.log = log;
        inner.context = context;
        let _ = self.live.send(session_event(&inner.session_id));
        for event in inner.log.iter().chain(&inner.context) {
            let _ = self.live.send(event.clone());
        }
    }

    /// Tells the open pages to reload the session list.
    fn sessions_changed(&self) {
        let _ = self.live.send(json!({"type": "sessions"}));
    }

    /// Writes the current session to disk (without busy markers, which are only live).
    fn save(&self, history: &[rig_core::message::Message]) -> Result<(), String> {
        let (id, log, context) = {
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
                .collect();
            (id, log, inner.context.clone())
        };
        self.store
            .save(&id, history, &log, context.as_ref())
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
            self.push(json!({"type": "question", "id": id, "status": "answered", "answer": answer}));
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
fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Cuts `text` to [`MAX_OUTPUT`] bytes at a character boundary.
fn clip(mut text: String) -> String {
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
fn tool_event(call: &ToolCall, status: &str) -> Value {
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

impl Frontend for Web {
    fn emit(&self, event: Event) {
        match event {
            Event::Text(text) => self.push(json!({"type": "text", "text": text})),
            Event::Reasoning(text) => self.push(json!({"type": "reasoning", "text": text})),
            Event::Note(text) => self.push(json!({"type": "note", "text": text})),
            Event::ToolStarted(call) => self.push(tool_event(&call, "running")),
            Event::ToolDenied(call) => self.push(tool_event(&call, "denied")),
            Event::ToolFinished {
                id, output, failed, ..
            } => self.push(json!({
                "type": "tool",
                "id": id,
                "status": if failed { "failed" } else { "done" },
                "output": clip(output),
            })),
            Event::Context(usage) => self.set_context(usage),
            Event::SubagentStarted {
                agent,
                description,
                prompt,
            } => self.push(json!({
                "type": "subagent",
                "agent": agent,
                "description": description,
                "prompt": clip(prompt),
                "status": "running",
            })),
            Event::SubagentFinished { agent, failed } => self.push(json!({
                "type": "subagent",
                "agent": agent,
                "status": if failed { "failed" } else { "done" },
            })),
            // The subagent's context is not the one the header shows.
            Event::Sub { agent, event } => match *event {
                Event::Text(text) => {
                    self.push(json!({"type": "text", "agent": agent, "text": text}))
                }
                Event::Reasoning(text) => {
                    self.push(json!({"type": "reasoning", "agent": agent, "text": text}))
                }
                Event::Note(text) => {
                    self.push(json!({"type": "note", "agent": agent, "text": text}))
                }
                _ => {}
            },
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

async fn authorize(State(web): State<Arc<Web>>, request: Request, next: Next) -> Response {
    let from_header = request
        .headers()
        .get("x-token")
        .and_then(|value| value.to_str().ok());
    let from_query = request.uri().query().and_then(|query| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
    });
    if from_header
        .or(from_query)
        .is_some_and(|given| token_matches(given, &web.token))
    {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn page() -> Html<&'static str> {
    Html(PAGE)
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
    let Some(thinking) = &web.thinking else {
        return StatusCode::NOT_FOUND;
    };
    let level = body["level"].as_str().unwrap_or_default();
    if !thinking.levels.iter().any(|l| l == level) {
        return StatusCode::BAD_REQUEST;
    }
    let mut inner = web.inner.lock().unwrap();
    inner.think = Some(level.to_owned());
    // Sent under the lock like the other state events.
    if let Some(event) = web.thinking_event(&inner) {
        let _ = web.live.send(event);
    }
    StatusCode::NO_CONTENT
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
        .route("/sessions", get(list_sessions))
        .route("/session", post(open_session))
        .route("/session/delete", post(delete_session))
        .layer(middleware::from_fn_with_state(web.clone(), authorize))
        .with_state(web);
    Router::new().route("/", get(page)).merge(api)
}

/// Runs what the browser asks for, one at a time. A cancelled prompt is dropped
/// and leaves the session's history unchanged.
async fn worker(web: Arc<Web>, mut session: Session, mut inbox: Inbox) {
    while let Some(command) = inbox.recv().await {
        match command {
            Command::Prompt(input) => {
                session.set_thinking(web.think_param());
                tokio::select! {
                    result = session.prompt(input) => {
                        if let Err(error) = result {
                            web.push(json!({"type": "error", "text": error}));
                        }
                    }
                    _ = web.cancel.notified() => {
                        web.cancel_approvals();
                        web.push(json!({"type": "note", "text": "stopped"}));
                    }
                }
                if let Err(error) = web.save(session.history()) {
                    web.push(json!({"type": "error", "text": error}));
                }
                web.sessions_changed();
            }
            Command::Switch(id) => switch(&web, &mut session, id),
            Command::Delete(id) => {
                let current = web.inner.lock().unwrap().session_id.clone();
                match web.store.delete(&id) {
                    Ok(()) if current.as_deref() == Some(id.as_str()) => {
                        switch(&web, &mut session, None)
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
}

/// Makes `id` (or a fresh session) the one the agent continues and the pages show.
fn switch(web: &Web, session: &mut Session, id: Option<String>) {
    let Some(id) = id else {
        session.set_history(Vec::new());
        web.replace(None, Vec::new(), None);
        return;
    };
    match web.store.load(&id) {
        Ok(saved) => {
            let mut log = saved.log;
            settle(&mut log);
            session.set_history(saved.history);
            web.replace(Some(id), log, saved.context);
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
        let (web, inbox) = Web::new(temp_store(), None).unwrap();
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
        let (web, _inbox) = Web::new(temp_store(), None).unwrap();
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
        let (web, _inbox) = Web::new(temp_store(), None).unwrap();
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
        assert_eq!(
            replayed,
            [
                json!({"type": "session", "id": null}),
                json!({"type": "text", "text": "hi"}),
                latest
            ]
        );
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
        let denied = request(addr, "POST /prompt", None, r#"{"text":"hi"}"#).await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        let wrong = request(addr, "POST /prompt", Some("nope"), r#"{"text":"hi"}"#).await;
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        let empty = request(addr, "POST /prompt", Some(&web.token), r#"{"text":"  "}"#).await;
        assert!(empty.starts_with("HTTP/1.1 400"), "{empty}");
        assert!(log(&web).is_empty());
    }

    /// The slider only exists for models that list levels, and only those can be picked.
    #[tokio::test]
    async fn thinking_level_is_chosen_from_the_models_levels() {
        let thinking = Thinking {
            levels: vec!["off".into(), "low".into(), "xhigh".into()],
            default: "low".into(),
        };
        let (web, _inbox) = Web::new(temp_store(), Some(thinking)).unwrap();
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
            "POST /thinking",
            Some(&web.token),
            r#"{"level":"max"}"#,
        )
        .await;
        assert!(unknown.starts_with("HTTP/1.1 400"), "{unknown}");
        let ok = request(
            addr,
            "POST /thinking",
            Some(&web.token),
            r#"{"level":"xhigh"}"#,
        )
        .await;
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert_eq!(web.think_param(), Some(json!("xhigh")));
        let off = request(
            addr,
            "POST /thinking",
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
            "POST /thinking",
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
            "GET /events?token={} HTTP/1.1\r\nHost: x\r\n\r\n",
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
        let response = request(addr, "POST /prompt", Some(&web.token), r#"{"text":"hi"}"#).await;
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
        request(addr, "POST /prompt", Some(&web.token), r#"{"text":"go"}"#).await;
        wait_for(&web, |e| e["type"] == "question" && e["status"] == "pending").await;
        let body = r#"{"id":"q0","answer":"blue"}"#;
        let ok = request(addr, "POST /answer", Some(&web.token), body).await;
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        let again = request(addr, "POST /answer", Some(&web.token), body).await;
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
        request(addr, "POST /prompt", Some(&web.token), r#"{"text":"run"}"#).await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "pending").await;
        let busy = request(
            addr,
            "POST /prompt",
            Some(&web.token),
            r#"{"text":"again"}"#,
        )
        .await;
        assert!(busy.starts_with("HTTP/1.1 409"), "{busy}");
        let body = r#"{"id":"t1","allow":true}"#;
        let answered = request(addr, "POST /approve", Some(&web.token), body).await;
        assert!(answered.starts_with("HTTP/1.1 204"), "{answered}");
        let unknown = request(addr, "POST /approve", Some(&web.token), body).await;
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
        request(addr, "POST /prompt", Some(&web.token), r#"{"text":"go"}"#).await;
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
        request(addr, "POST /approve", Some(&web.token), &body).await;
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
        request(addr, "POST /prompt", Some(&web.token), r#"{"text":"run"}"#).await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "pending").await;
        request(
            addr,
            "POST /approve",
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
        request(addr, "POST /prompt", Some(&web.token), r#"{"text":"run"}"#).await;
        wait_for(&web, |e| e["id"] == "t1" && e["status"] == "pending").await;
        request(addr, "POST /cancel", Some(&web.token), "").await;
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
        request(addr, "POST /prompt", token, r#"{"text":"first chat"}"#).await;
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        let listed = request(addr, "GET /sessions", token, "").await;
        assert!(listed.contains("first chat"), "{listed}");
        let id = web.inner.lock().unwrap().session_id.clone().unwrap();
        assert!(web.store.dir().join(format!("{id}.json")).exists());

        // A new session starts empty ...
        let started = request(addr, "POST /session", token, "{}").await;
        assert!(started.starts_with("HTTP/1.1 202"), "{started}");
        for _ in 0..500 {
            if log(&web).iter().all(|e| e["type"] == "status") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(web.inner.lock().unwrap().session_id.is_none());
        request(addr, "POST /prompt", token, r#"{"text":"second chat"}"#).await;
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "two").await;
        wait_for(&web, |e| e["type"] == "status" && e["busy"] == false).await;
        assert_eq!(web.store.list().len(), 2);

        // ... and the first one can be opened again with its transcript.
        let body = format!(r#"{{"id":"{id}"}}"#);
        request(addr, "POST /session", token, &body).await;
        wait_for(&web, |e| e["type"] == "text" && e["text"] == "one").await;
        assert!(!log(&web).iter().any(|e| e["text"] == "two"));
        let bad = request(addr, "POST /session", token, r#"{"id":"../x"}"#).await;
        assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");

        request(addr, "POST /session/delete", token, &body).await;
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
