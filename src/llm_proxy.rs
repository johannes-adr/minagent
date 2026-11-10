//! Localhost proxy to the LLM API for sandboxed subagents.
//!
//! The sandbox only allows connections to localhost and hides `.env`, so a subagent can't
//! reach the model itself. The parent runs this proxy on `127.0.0.1`; it accepts requests
//! that carry the random token as API key (`Authorization: Bearer <token>` or
//! `x-api-key: <token>`), replaces it with the real credential and forwards them to the
//! configured base URL's origin, and no other host. Responses (also SSE streams) are
//! passed through as they arrive. HTTP itself is handled by hyper (server) and reqwest.

use crate::config::Upstream;
use futures::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Body as _, Bytes, Frame, Incoming};
use hyper::header::{AUTHORIZATION, CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::convert::Infallible;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

/// Largest accepted request body (prompts with history and images stay well below).
const MAX_BODY: usize = 64 * 1024 * 1024;
/// Time a client gets to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

type Body = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

struct State {
    /// `scheme://host[:port]` of the real base URL.
    origin: String,
    auth: Option<(&'static str, String)>,
    token: String,
    client: reqwest::Client,
}

/// Starts the proxy in the background; returns its URL (`http://127.0.0.1:<port>`) and token.
pub async fn start(upstream: Upstream) -> io::Result<(String, String)> {
    let url = reqwest::Url::parse(&upstream.base_url)
        .map_err(|e| io::Error::other(format!("invalid base URL {:?}: {e}", upstream.base_url)))?;
    let token = random_token()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy = format!("http://{}", listener.local_addr()?);
    let state = Arc::new(State {
        origin: url.origin().ascii_serialization(),
        auth: upstream.auth,
        token: token.clone(),
        client: reqwest::Client::new(),
    });
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let state = state.clone();
            let service = hyper::service::service_fn(move |request| {
                let state = state.clone();
                async move {
                    let response = forward(request, &state).await;
                    Ok::<_, Infallible>(response.unwrap_or_else(error_response))
                }
            });
            let connection = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(REQUEST_TIMEOUT)
                .keep_alive(false)
                .serve_connection(TokioIo::new(socket), service);
            tokio::spawn(connection);
        }
    });
    Ok((proxy, token))
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

type Failure = (StatusCode, &'static str);

fn error_response((status, message): Failure) -> Response<Body> {
    let body = Full::new(Bytes::from(message)).map_err(|e| match e {}).boxed();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

async fn forward(request: Request<Incoming>, state: &State) -> Result<Response<Body>, Failure> {
    let (parts, body) = request.into_parts();
    let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
    let authorized = header("authorization") == Some(&format!("Bearer {}", state.token))
        || header("x-api-key") == Some(state.token.as_str());
    if !authorized {
        return Err((StatusCode::UNAUTHORIZED, "missing or wrong proxy token"));
    }

    // Only the configured host gets the real credential: the target must be a path, and
    // the resulting URL must keep the upstream origin.
    let path = parts.uri.path_and_query().map_or("/", |p| p.as_str());
    let url = reqwest::Url::parse(&format!("{}{path}", state.origin))
        .ok()
        .filter(|url| {
            parts.uri.authority().is_none()
                && path.starts_with('/')
                && url.origin().ascii_serialization() == state.origin
        })
        .ok_or((StatusCode::BAD_REQUEST, "the request target must be a path"))?;

    if body.size_hint().lower() > MAX_BODY as u64 {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "request body too large"));
    }
    let body = tokio::time::timeout(REQUEST_TIMEOUT, Limited::new(body, MAX_BODY).collect())
        .await
        .map_err(|_| (StatusCode::REQUEST_TIMEOUT, "request body not received in time"))?
        .map_err(|_| (StatusCode::BAD_REQUEST, "request body too large or unreadable"))?
        .to_bytes();

    let mut headers = parts.headers;
    for name in [HOST, AUTHORIZATION, CONTENT_LENGTH, CONNECTION, TRANSFER_ENCODING] {
        headers.remove(name);
    }
    headers.remove("x-api-key");
    let mut upstream = state
        .client
        .request(parts.method, url)
        .headers(headers)
        .body(body);
    if let Some((name, value)) = &state.auth {
        upstream = upstream.header(*name, value);
    }
    let upstream = upstream
        .send()
        .await
        .map_err(|_| (StatusCode::BAD_GATEWAY, "the LLM API is unreachable"))?;

    // Streamed back without a length, i.e. chunked: if the upstream breaks off, hyper
    // ends the connection without the final chunk and the client sees an error.
    let mut response = Response::builder().status(upstream.status());
    for (name, value) in upstream.headers() {
        if ![TRANSFER_ENCODING, CONTENT_LENGTH, CONNECTION].contains(name) {
            response = response.header(name, value);
        }
    }
    let stream = upstream
        .bytes_stream()
        .map_ok(Frame::data)
        .map_err(|e| Box::new(e) as _);
    response
        .body(BodyExt::boxed(StreamBody::new(stream)))
        .map_err(|_| (StatusCode::BAD_GATEWAY, "invalid response from the LLM API"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// A fake LLM API: answers one request with an SSE body in two parts and returns the
    /// request head it saw.
    async fn fake_upstream() -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut chunk = [0u8; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf).into_owned();
            let response = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                            transfer-encoding: chunked\r\n\r\n6\r\ndata: \r\n";
            socket.write_all(response.as_bytes()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            socket.write_all(b"3\r\nhi\n\r\n0\r\n\r\n").await.unwrap();
            head
        });
        (url, task)
    }

    #[tokio::test]
    async fn forwards_with_the_real_credential() {
        let (url, upstream) = fake_upstream().await;
        let (proxy, token) = start(Upstream {
            base_url: url,
            auth: Some(("authorization", "Bearer sk-real".into())),
        })
        .await
        .unwrap();
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/responses"))
            .bearer_auth(&token)
            .body("{\"stream\":true}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.text().await.unwrap(), "data: hi\n");
        let head = upstream.await.unwrap().to_lowercase();
        assert!(head.starts_with("post /v1/responses "), "{head}");
        assert!(head.contains("authorization: bearer sk-real"), "{head}");
        assert!(!head.contains(&token), "{head}");
    }

    #[tokio::test]
    async fn rejects_requests_without_the_token() {
        let (proxy, _token) = start(Upstream {
            base_url: "http://127.0.0.1:9".into(),
            auth: None,
        })
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let response = client.get(format!("{proxy}/v1/models")).send().await.unwrap();
        assert_eq!(response.status(), 401);
        let response = client
            .get(format!("{proxy}/v1/models"))
            .header("x-api-key", "wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }

    /// Sends a raw request (so the request target can be anything) and returns the response.
    async fn raw_request(proxy: &str, head: &str) -> String {
        let mut socket = TcpStream::connect(proxy.trim_start_matches("http://"))
            .await
            .unwrap();
        socket.write_all(head.as_bytes()).await.unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        response
    }

    /// The request target is appended to the upstream origin, so `@host` or `.host` must
    /// not redirect the request (and the real key) to another host.
    #[tokio::test]
    async fn request_target_cannot_change_the_host() {
        let attacker = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let attacker_addr = attacker.local_addr().unwrap();
        let (proxy, token) = start(Upstream {
            base_url: "http://localhost:1/v1".into(),
            auth: Some(("authorization", "Bearer sk-real".into())),
        })
        .await
        .unwrap();
        for target in [format!("@{attacker_addr}/x"), ".evil.example/x".into()] {
            let head = format!("GET {target} HTTP/1.1\r\nauthorization: Bearer {token}\r\n\r\n");
            let response = raw_request(&proxy, &head).await;
            assert!(response.starts_with("HTTP/1.1 400"), "{target}: {response}");
        }
        let connected =
            tokio::time::timeout(std::time::Duration::from_millis(200), attacker.accept()).await;
        assert!(connected.is_err(), "the proxy connected to another host");
    }

    /// A huge `content-length` must be refused, not allocated.
    #[tokio::test]
    async fn refuses_oversized_bodies() {
        let (proxy, token) = start(Upstream {
            base_url: "http://127.0.0.1:9".into(),
            auth: None,
        })
        .await
        .unwrap();
        let head = format!(
            "POST /v1/x HTTP/1.1\r\nauthorization: Bearer {token}\r\ncontent-length: 1000000000000\r\n\r\n"
        );
        let response = raw_request(&proxy, &head).await;
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    }

    /// When the upstream breaks off mid-stream, the client must see an error, not a
    /// shorter but seemingly complete answer.
    #[tokio::test]
    async fn truncated_upstream_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut chunk = [0u8; 4096];
            let _ = socket.read(&mut chunk).await.unwrap();
            let response = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n6\r\ndata: \r\n";
            socket.write_all(response.as_bytes()).await.unwrap();
            // Closes without the terminating chunk.
        });
        let (proxy, token) = start(Upstream { base_url: url, auth: None }).await.unwrap();
        let response = reqwest::Client::new()
            .post(format!("{proxy}/v1/x"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.text().await.is_err());
    }
}
