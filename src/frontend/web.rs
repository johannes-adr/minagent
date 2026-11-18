//! Web frontend: `agent --web [addr]` serves a chat page (default `127.0.0.1:8080`).
//!
//! The page ([`web.html`](web.html)) is one self-contained file. It gets the session as a
//! stream of JSON events over server-sent events (`GET /events`) and talks back with
//! `POST /prompt`, `POST /approve` and `POST /cancel`. Like the terminal UI there is one
//! session; every browser tab shows it, and a reloaded tab gets the whole transcript replayed.
//!
//! Whoever can call the API can make the agent run commands, so all of it needs the random
//! token printed at startup (header `x-token`, or `?token=` for the event stream). Only the
//! static page is served without it.

use crate::frontend::{Event, Frontend, ToolCall};
use crate::harness::Session;
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
}

/// The web frontend's state, shared by the harness side ([`Frontend`]) and the HTTP handlers.
pub struct Web {
    token: String,
    inner: Mutex<Inner>,
    live: broadcast::Sender<Value>,
    busy: AtomicBool,
    prompts: mpsc::UnboundedSender<String>,
    cancel: Notify,
}

impl Web {
    /// Returns the frontend and the receiving end of the prompts the browser submits,
    /// which [`run`] feeds to the session.
    pub fn new() -> io::Result<(Arc<Self>, mpsc::UnboundedReceiver<String>)> {
        let (prompts, inbox) = mpsc::unbounded_channel();
        let web = Self {
            token: random_token()?,
            inner: Mutex::default(),
            live: broadcast::channel(4096).0,
            busy: AtomicBool::new(false),
            prompts,
            cancel: Notify::new(),
        };
        Ok((Arc::new(web), inbox))
    }

    /// Records `event` and sends it to the connected browsers.
    fn push(&self, event: Value) {
        let mut inner = self.inner.lock().unwrap();
        if event["type"] == "text"
            && let Some(last) = inner.log.last_mut()
            && last["type"] == "text"
            && let Some(Value::String(text)) = last.get_mut("text")
        {
            text.push_str(event["text"].as_str().unwrap_or_default());
        } else {
            inner.log.push(event.clone());
        }
        // Sent under the lock so that `subscribe` sees each event exactly once.
        let _ = self.live.send(event);
    }

    /// The events so far and a receiver for all later ones.
    fn subscribe(&self) -> (Vec<Value>, broadcast::Receiver<Value>) {
        let inner = self.inner.lock().unwrap();
        (inner.log.clone(), self.live.subscribe())
    }

    /// Answers a pending approval; `false` if there is none with this id.
    fn answer(&self, id: &str, allow: bool) -> bool {
        let reply = self.inner.lock().unwrap().approvals.remove(id);
        reply.is_some_and(|reply| reply.send(allow).is_ok())
    }

    /// Marks every unanswered approval as cancelled (the prompt that asked is gone).
    fn cancel_approvals(&self) {
        let ids: Vec<String> = {
            let mut inner = self.inner.lock().unwrap();
            inner.approvals.drain().map(|(id, _)| id).collect()
        };
        for id in ids {
            self.push(json!({"type": "tool", "id": id, "status": "cancelled"}));
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
        self.push(json!({"type": "status", "busy": busy}));
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
    })
}

impl Frontend for Web {
    fn emit(&self, event: Event) {
        match event {
            Event::Text(text) => self.push(json!({"type": "text", "text": text})),
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
    if web.prompts.send(text).is_err() {
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
        .route("/cancel", post(cancel))
        .layer(middleware::from_fn_with_state(web.clone(), authorize))
        .with_state(web);
    Router::new().route("/", get(page)).merge(api)
}

/// Runs the prompts the browser submits, one at a time. A cancelled prompt is dropped
/// and leaves the session's history unchanged.
async fn worker(web: Arc<Web>, mut session: Session, mut inbox: mpsc::UnboundedReceiver<String>) {
    while let Some(input) = inbox.recv().await {
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
        web.set_busy(false);
    }
}

/// Serves the page on `addr` until Ctrl-C. `session` must have been created with `web`
/// as its frontend and `inbox` is the receiver from [`Web::new`].
pub async fn run(
    web: Arc<Web>,
    inbox: mpsc::UnboundedReceiver<String>,
    session: Session,
    addr: &str,
) -> io::Result<()> {
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

    fn turn(event: MockStreamEvent) -> Vec<MockStreamEvent> {
        vec![event, MockStreamEvent::final_response_with_total_tokens(1)]
    }

    /// A server whose model answers with `turns`.
    async fn start(turns: Vec<Vec<MockStreamEvent>>) -> (Arc<Web>, SocketAddr) {
        let (web, inbox) = Web::new().unwrap();
        let model = MockCompletionModel::from_stream_turns(turns);
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
        let (web, _inbox) = Web::new().unwrap();
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
}
