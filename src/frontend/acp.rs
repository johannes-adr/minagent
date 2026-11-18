//! Agent Client Protocol (ACP) frontend: the agent talks JSON-RPC over stdin/stdout to
//! an ACP client such as Zed (`agent --acp`). See <https://agentclientprotocol.com>.
//!
//! Supported: `initialize`, `session/new`, `session/prompt` (text, resource links and
//! embedded resources), `session/cancel`, streamed answer text, tool call reports and
//! `session/request_permission` for calls that need approval, and `usage_update` with the
//! tokens in context after each model call (only when the context window is known, as the
//! update requires its size). A cancelled prompt is dropped from the session's history.

use crate::frontend::{Event, Frontend, ToolCall};
use crate::harness::Session;
use futures::future::BoxFuture;
use rig_core::serde_json::{self, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, oneshot};

const PROTOCOL_VERSION: u64 = 1;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const PARSE_ERROR: i64 = -32700;

/// The JSON-RPC connection to the client: outgoing messages and our pending requests.
struct Connection {
    out: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
}

impl Connection {
    fn send(&self, message: Value) {
        let _ = self.out.send(message.to_string());
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn respond(&self, id: Value, result: Result<Value, (i64, String)>) {
        self.send(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        });
    }

    /// Sends a request to the client; `None` if it answers with an error or never answers.
    async fn request(&self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, reply);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        answer.await.ok()?.get("result").cloned()
    }

    /// Hands a response from the client to the request waiting for it.
    fn resolve(&self, message: Value) {
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return;
        };
        if let Some(reply) = self.pending.lock().unwrap().remove(&id) {
            let _ = reply.send(message);
        }
    }
}

/// Reports one session's events as `session/update` notifications.
struct AcpFrontend {
    conn: Arc<Connection>,
    session_id: String,
    /// Tool calls already reported (as `pending`, before asking for permission).
    announced: Mutex<HashSet<String>>,
}

/// The ACP fields describing `call`. Multi-line titles (code) go into the content,
/// the first line is the title.
fn tool_call_fields(call: &ToolCall) -> serde_json::Map<String, Value> {
    let title = call.title.lines().next().unwrap_or_default();
    let mut fields = json!({
        "toolCallId": call.id,
        "title": title,
        "kind": call.kind,
        "rawInput": call.args,
    });
    if call.title.contains('\n') {
        fields["content"] = json!([text_content(&call.title)]);
    }
    match fields {
        Value::Object(fields) => fields,
        _ => unreachable!(),
    }
}

fn text_content(text: &str) -> Value {
    json!({"type": "content", "content": {"type": "text", "text": text}})
}

impl AcpFrontend {
    fn update(&self, update: Value) {
        let params = json!({"sessionId": self.session_id, "update": update});
        self.conn.notify("session/update", params);
    }

    fn tool_call(&self, call: &ToolCall, status: &str) {
        let mut fields = tool_call_fields(call);
        fields.insert("sessionUpdate".into(), "tool_call".into());
        fields.insert("status".into(), status.into());
        self.update(Value::Object(fields));
    }

    fn announced(&self, id: &str) -> bool {
        self.announced.lock().unwrap().remove(id)
    }
}

impl Frontend for AcpFrontend {
    fn emit(&self, event: Event) {
        match event {
            Event::Text(text) => self.update(json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text},
            })),
            Event::Note(text) => self.update(json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": format!("{text}\n")},
            })),
            Event::ToolStarted(call) => {
                if self.announced(&call.id) {
                    self.update(json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call.id,
                        "status": "in_progress",
                    }));
                } else {
                    self.tool_call(&call, "in_progress");
                }
            }
            Event::ToolDenied(call) => {
                self.announced(&call.id);
                self.update(json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": call.id,
                    "status": "failed",
                    "content": [text_content("The user denied this tool call.")],
                }));
            }
            Event::ToolFinished {
                id, output, failed, ..
            } => self.update(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "status": if failed { "failed" } else { "completed" },
                "content": [text_content(&output)],
            })),
            Event::Context(usage) => {
                if let Some(size) = usage.window {
                    self.update(json!({
                        "sessionUpdate": "usage_update",
                        "used": usage.tokens,
                        "size": size,
                    }));
                }
            }
        }
    }

    fn approve(&self, call: &ToolCall) -> BoxFuture<'static, bool> {
        self.announced.lock().unwrap().insert(call.id.clone());
        self.tool_call(call, "pending");
        let params = json!({
            "sessionId": self.session_id,
            "toolCall": tool_call_fields(call),
            "options": [
                {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
            ],
        });
        let conn = self.conn.clone();
        Box::pin(async move {
            let Some(result) = conn.request("session/request_permission", params).await else {
                return false;
            };
            let outcome = &result["outcome"];
            outcome["outcome"] == "selected" && outcome["optionId"] == "allow"
        })
    }
}

/// The prompt's content blocks as one text for the model.
fn prompt_text(blocks: &Value) -> String {
    let block_text = |block: &Value| -> Option<String> {
        match block["type"].as_str()? {
            "text" => block["text"].as_str().map(str::to_owned),
            "resource_link" => Some(format!(
                "[{}]({})",
                block["name"].as_str().unwrap_or(""),
                block["uri"].as_str()?
            )),
            "resource" => {
                let resource = &block["resource"];
                let uri = resource["uri"].as_str().unwrap_or_default();
                Some(match resource["text"].as_str() {
                    Some(text) => format!("<file uri=\"{uri}\">\n{text}\n</file>"),
                    None => format!("[{uri}]({uri})"),
                })
            }
            _ => None,
        }
    };
    let parts: Vec<String> = blocks
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(block_text)
        .collect();
    parts.join("\n")
}

struct SessionState {
    session: tokio::sync::Mutex<Session>,
    cancel: Notify,
}

/// Serves ACP on stdin/stdout until stdin closes.
pub async fn serve(new_session: impl Fn(Arc<dyn Frontend>) -> Session) -> std::io::Result<()> {
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    serve_io(stdin, tokio::io::stdout(), new_session).await
}

/// Serves ACP on `reader`/`writer`; `new_session` creates the session for `session/new`.
async fn serve_io(
    reader: impl AsyncBufRead + Unpin,
    mut writer: impl AsyncWrite + Unpin + Send + 'static,
    new_session: impl Fn(Arc<dyn Frontend>) -> Session,
) -> std::io::Result<()> {
    let (out, mut outbox) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = outbox.recv().await {
            let written = async {
                writer.write_all(line.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            };
            if written.await.is_err() {
                return;
            }
        }
    });
    let conn = Arc::new(Connection {
        out,
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(0),
    });
    let mut sessions: HashMap<String, Arc<SessionState>> = HashMap::new();
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            conn.respond(Value::Null, Err((PARSE_ERROR, "invalid JSON".into())));
            continue;
        };
        let Some(method) = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            conn.resolve(message);
            continue;
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let session = params["sessionId"]
            .as_str()
            .and_then(|s| sessions.get(s))
            .cloned();
        match (method.as_str(), id) {
            ("initialize", Some(id)) => conn.respond(
                id,
                Ok(json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "agentCapabilities": {
                        "loadSession": false,
                        "promptCapabilities": {"image": false, "audio": false, "embeddedContext": true},
                    },
                    "agentInfo": {"name": "agent", "version": env!("CARGO_PKG_VERSION")},
                    "authMethods": [],
                })),
            ),
            ("session/new", Some(id)) => {
                // Tools work in the current directory, so follow the client's project.
                if let Some(cwd) = params["cwd"].as_str().filter(|cwd| Path::new(cwd).is_dir()) {
                    let _ = std::env::set_current_dir(cwd);
                }
                let session_id = format!("sess_{}", sessions.len() + 1);
                let frontend = Arc::new(AcpFrontend {
                    conn: conn.clone(),
                    session_id: session_id.clone(),
                    announced: Mutex::new(HashSet::new()),
                });
                let state = SessionState {
                    session: tokio::sync::Mutex::new(new_session(frontend)),
                    cancel: Notify::new(),
                };
                sessions.insert(session_id.clone(), Arc::new(state));
                conn.respond(id, Ok(json!({"sessionId": session_id})));
            }
            ("session/prompt", Some(id)) => {
                let Some(state) = session else {
                    conn.respond(id, Err((INVALID_PARAMS, "unknown sessionId".into())));
                    continue;
                };
                let text = prompt_text(&params["prompt"]);
                let conn = conn.clone();
                tokio::spawn(async move {
                    let mut session = state.session.lock().await;
                    let result = tokio::select! {
                        result = session.prompt(text) => result.map(|()| "end_turn"),
                        () = state.cancel.notified() => Ok("cancelled"),
                    };
                    let result = result
                        .map(|reason| json!({"stopReason": reason}))
                        .map_err(|error| (INTERNAL_ERROR, error));
                    conn.respond(id, result);
                });
            }
            ("session/cancel", None) => {
                if let Some(state) = session {
                    state.cancel.notify_waiters();
                }
            }
            (method, Some(id)) => {
                conn.respond(id, Err((METHOD_NOT_FOUND, format!("unknown method {method}"))))
            }
            // Unknown notifications are ignored.
            (_, None) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
    use tokio::io::{BufReader, DuplexStream, Lines};

    struct Client {
        to_agent: DuplexStream,
        from_agent: Lines<BufReader<DuplexStream>>,
    }

    impl Client {
        /// Starts an agent whose model answers with `turns`.
        fn start(turns: Vec<Vec<MockStreamEvent>>) -> Self {
            Self::start_with_window(turns, None)
        }

        /// [`Self::start`] for a model whose context window is `window`.
        fn start_with_window(turns: Vec<Vec<MockStreamEvent>>, window: Option<u64>) -> Self {
            let (to_agent, agent_in) = tokio::io::duplex(1 << 16);
            let (agent_out, from_agent) = tokio::io::duplex(1 << 16);
            let model = MockCompletionModel::from_stream_turns(turns);
            tokio::spawn(serve_io(
                BufReader::new(agent_in),
                agent_out,
                move |frontend| {
                    Session::new(model.clone().into(), "", frontend).with_context_window(window)
                },
            ));
            Self {
                to_agent,
                from_agent: BufReader::new(from_agent).lines(),
            }
        }

        async fn send(&mut self, message: Value) {
            let line = format!("{message}\n");
            self.to_agent.write_all(line.as_bytes()).await.unwrap();
        }

        async fn next(&mut self) -> Value {
            let line = self
                .from_agent
                .next_line()
                .await
                .unwrap()
                .expect("agent closed");
            serde_json::from_str(&line).unwrap()
        }

        /// Reads messages up to and including the response to request `id`.
        async fn until_response(&mut self, id: u64) -> Vec<Value> {
            let mut messages = Vec::new();
            loop {
                let message = self.next().await;
                let done = message["id"] == id && message.get("method").is_none();
                messages.push(message);
                if done {
                    return messages;
                }
            }
        }

        async fn new_session(&mut self) -> String {
            self.send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": 1}}))
                .await;
            let init = self.next().await;
            assert_eq!(init["result"]["protocolVersion"], 1, "{init}");
            self.send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/new", "params": {"cwd": ".", "mcpServers": []}}))
                .await;
            let created = self.next().await;
            created["result"]["sessionId"].as_str().unwrap().to_owned()
        }

        async fn prompt(&mut self, session: &str, text: &str) {
            self.send(
                json!({"jsonrpc": "2.0", "id": 2, "method": "session/prompt", "params": {
                    "sessionId": session, "prompt": [{"type": "text", "text": text}],
                }}),
            )
            .await;
        }
    }

    fn turn(event: MockStreamEvent) -> Vec<MockStreamEvent> {
        vec![event, MockStreamEvent::final_response_with_total_tokens(1)]
    }

    /// Outside an isolated container `execute` needs approval.
    fn echo() -> Vec<MockStreamEvent> {
        let args = json!({"command": "echo hi"});
        turn(MockStreamEvent::tool_call("t1", "execute", args))
    }

    fn updates(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .filter(|m| m["method"] == "session/update")
            .map(|m| {
                let update = &m["params"]["update"];
                let kind = update["sessionUpdate"].as_str().unwrap();
                match update["status"].as_str() {
                    Some(status) => format!("{kind} {status}"),
                    None => format!(
                        "{kind} {}",
                        update["content"]["text"].as_str().unwrap_or("")
                    ),
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn prompt_streams_the_answer() {
        let mut client = Client::start(vec![turn(MockStreamEvent::text("hello"))]);
        let session = client.new_session().await;
        client.prompt(&session, "hi").await;
        let messages = client.until_response(2).await;
        assert_eq!(updates(&messages), ["agent_message_chunk hello"]);
        assert_eq!(messages.last().unwrap()["result"]["stopReason"], "end_turn");
    }

    /// After each model call the client gets the tokens in context and the window size;
    /// without a known window there is no size to send, so no update.
    #[tokio::test]
    async fn usage_is_reported_when_the_window_is_known() {
        use rig_core::completion::Usage;
        for (window, expected) in [(Some(200_000), 1), (None, 0)] {
            let usage = Usage::new().input_tokens(40_000).output_tokens(2_000);
            let answer = vec![
                MockStreamEvent::text("hello"),
                MockStreamEvent::final_response(usage),
            ];
            let mut client = Client::start_with_window(vec![answer], window);
            let session = client.new_session().await;
            client.prompt(&session, "hi").await;
            let messages = client.until_response(2).await;
            let usage: Vec<_> = messages
                .iter()
                .map(|m| &m["params"]["update"])
                .filter(|u| u["sessionUpdate"] == "usage_update")
                .collect();
            assert_eq!(usage.len(), expected, "{messages:?}");
            if let Some(update) = usage.first() {
                assert_eq!(
                    (&update["used"], &update["size"]),
                    (&json!(42_000), &json!(200_000))
                );
            }
        }
    }

    /// A call that needs approval is reported, the client is asked, and the result is reported.
    #[tokio::test]
    async fn permission_is_requested_and_tool_result_reported() {
        let mut client = Client::start(vec![echo(), turn(MockStreamEvent::text("done"))]);
        let session = client.new_session().await;
        client.prompt(&session, "say hi").await;
        let mut messages = Vec::new();
        let request = loop {
            let message = client.next().await;
            if message["method"] == "session/request_permission" {
                break message;
            }
            messages.push(message);
        };
        assert_eq!(request["params"]["toolCall"]["toolCallId"], "t1");
        assert_eq!(request["params"]["toolCall"]["title"], "execute: echo hi");
        client
            .send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}))
            .await;
        messages.extend(client.until_response(2).await);
        let updates = updates(&messages);
        assert_eq!(
            updates,
            [
                "tool_call pending",
                "tool_call_update in_progress",
                "tool_call_update completed",
                "agent_message_chunk done",
            ]
        );
        let result = messages
            .iter()
            .find(|m| m["params"]["update"]["status"] == "completed")
            .unwrap();
        let output = result["params"]["update"]["content"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(output.contains("hi"), "{output}");
        assert_eq!(messages.last().unwrap()["result"]["stopReason"], "end_turn");
    }

    /// `session/cancel` while the client is being asked ends the prompt as `cancelled`.
    #[tokio::test]
    async fn cancel_ends_the_prompt() {
        let mut client = Client::start(vec![echo()]);
        let session = client.new_session().await;
        client.prompt(&session, "say hi").await;
        let request = loop {
            let message = client.next().await;
            if message["method"] == "session/request_permission" {
                break message;
            }
        };
        client
            .send(json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}))
            .await;
        client
            .send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {"outcome": {"outcome": "cancelled"}}}))
            .await;
        let messages = client.until_response(2).await;
        assert_eq!(
            messages.last().unwrap()["result"]["stopReason"],
            "cancelled"
        );
    }

    #[tokio::test]
    async fn unknown_methods_and_sessions_are_errors() {
        let mut client = Client::start(vec![]);
        client
            .send(json!({"jsonrpc": "2.0", "id": 7, "method": "nope"}))
            .await;
        assert_eq!(client.next().await["error"]["code"], METHOD_NOT_FOUND);
        client.prompt("missing", "hi").await;
        assert_eq!(client.next().await["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn prompt_blocks_become_text() {
        let blocks = json!([
            {"type": "text", "text": "look at"},
            {"type": "resource_link", "name": "a.rs", "uri": "file:///a.rs"},
            {"type": "resource", "resource": {"uri": "file:///b.rs", "text": "fn b() {}"}},
            {"type": "image", "data": "..."},
        ]);
        assert_eq!(
            prompt_text(&blocks),
            "look at\n[a.rs](file:///a.rs)\n<file uri=\"file:///b.rs\">\nfn b() {}\n</file>"
        );
    }
}
