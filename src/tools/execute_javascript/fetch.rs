//! `fetch` of `execute_javascript`: HTTP requests to the hosts the call declared.

use super::{Secret, Settings};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Url, redirect};
use rig_core::serde_json::{self, Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

/// Responses larger than this fail the request.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// The capability behind `fetch` for one script run.
pub(super) struct Fetch {
    /// Normalized hostnames the script may contact (the `hosts` argument).
    pub hosts: Vec<String>,
    pub settings: Arc<Settings>,
    /// The end of the run; requests don't outlive it.
    pub deadline: Instant,
    pub handle: Handle,
}

impl Fetch {
    /// Checks that `url` may be requested and that `secret` (if any) may be sent to it.
    fn authorize(&self, url: &str, secret: Option<&str>) -> Result<(Url, Option<&Secret>), String> {
        let url = Url::parse(url).map_err(|e| format!("invalid URL {url:?}: {e}"))?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let local = matches!(host.as_str(), "localhost" | "127.0.0.1");
        match url.scheme() {
            "https" => {}
            "http" if local => {}
            scheme => {
                return Err(format!(
                    "{scheme}:// is not allowed; use https:// (http:// only for localhost)"
                ));
            }
        }
        if !self.hosts.contains(&host) {
            return Err(format!(
                "host {host} was not declared; pass it in the `hosts` argument of execute_javascript"
            ));
        }
        let secret = secret
            .map(|name| {
                let secret = self
                    .settings
                    .secrets
                    .get(name)
                    .ok_or_else(|| format!("unknown secret {name:?}"))?;
                if secret.host == host {
                    Ok(secret)
                } else {
                    Err(format!(
                        "secret {name:?} may only be sent to {}, not to {host}",
                        secret.host
                    ))
                }
            })
            .transpose()?;
        Ok((url, secret))
    }

    /// Performs the request and returns `{status, headers, body}` as JSON. Blocks the
    /// calling (non-async) thread until the response is complete.
    pub fn call(
        &self,
        url: &str,
        method: &str,
        headers: &str,
        body: Option<String>,
        secret: Option<&str>,
    ) -> Result<String, String> {
        let (url, secret) = self.authorize(url, secret)?;
        let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .map_err(|_| format!("invalid method {method:?}"))?;
        let headers = request_headers(headers, secret)?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        self.handle
            .block_on(send(url, method, headers, body, remaining))
    }
}

/// The headers of the script plus the secret's `Authorization`, which replaces any the
/// script set.
fn request_headers(json: &str, secret: Option<&Secret>) -> Result<HeaderMap, String> {
    let given: HashMap<String, String> =
        serde_json::from_str(json).map_err(|e| format!("invalid headers: {e}"))?;
    let mut headers = HeaderMap::new();
    for (name, value) in given {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid header name {name:?}"))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|_| format!("invalid value for header {name}"))?;
        headers.append(name, value);
    }
    if let Some(secret) = secret {
        let mut value = HeaderValue::from_str(&secret.value)
            .map_err(|_| "the secret is not a valid header value".to_owned())?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    }
    Ok(headers)
}

async fn send(
    url: Url,
    method: Method,
    headers: HeaderMap,
    body: Option<String>,
    timeout: Duration,
) -> Result<String, String> {
    // No redirects: they could lead to a host the script didn't declare.
    let client = reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.request(method, url).headers(headers);
    if let Some(body) = body {
        request = request.body(body);
    }
    let mut response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let mut response_headers = Map::new();
    for name in response.headers().keys() {
        let values: Vec<_> = response
            .headers()
            .get_all(name)
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect();
        response_headers.insert(name.to_string(), Value::String(values.join(", ")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "response is larger than {} MB",
                MAX_RESPONSE_BYTES >> 20
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(json!({
        "status": status,
        "headers": response_headers,
        "body": String::from_utf8_lossy(&body),
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(hosts: &[&str], secrets: &[(&str, &str, &str)]) -> Fetch {
        let secrets = secrets
            .iter()
            .map(|(name, host, value)| {
                let secret = Secret {
                    host: host.to_string(),
                    value: value.to_string(),
                };
                (name.to_string(), secret)
            })
            .collect();
        Fetch {
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            settings: Arc::new(Settings {
                allowed_hosts: Vec::new(),
                secrets,
            }),
            deadline: Instant::now() + Duration::from_secs(10),
            handle: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .handle()
                .clone(),
        }
    }

    #[test]
    fn only_declared_hosts_over_https_or_localhost_http() {
        let f = fetch(&["api.example.com", "localhost"], &[]);
        assert!(f.authorize("https://api.example.com/x", None).is_ok());
        assert!(f.authorize("https://API.example.com/x", None).is_ok());
        assert!(f.authorize("http://localhost:8080/x", None).is_ok());
        let err = |url| f.authorize(url, None).err().unwrap();
        assert!(err("https://evil.com/").contains("not declared"));
        assert!(err("https://api.example.com.evil.com/").contains("not declared"));
        assert!(err("https://api.example.com@evil.com/").contains("not declared"));
        assert!(err("http://api.example.com/").contains("not allowed"));
        assert!(err("file:///etc/passwd").contains("not allowed"));
        assert!(err("not a url").contains("invalid URL"));
    }

    #[test]
    fn a_secret_goes_only_to_its_host() {
        let f = fetch(
            &["api.github.com", "other.com"],
            &[("GITHUB", "api.github.com", "Bearer ghp_x")],
        );
        let secret = f.authorize("https://api.github.com/", Some("GITHUB"));
        assert_eq!(secret.unwrap().1.unwrap().value, "Bearer ghp_x");
        let err = |url, name| f.authorize(url, Some(name)).err().unwrap();
        assert!(err("https://other.com/", "GITHUB").contains("may only be sent to api.github.com"));
        assert!(err("https://other.com/", "NOPE").contains("unknown secret"));
    }

    #[test]
    fn the_secret_replaces_a_script_authorization_header() {
        let secret = Secret {
            host: "h".into(),
            value: "Bearer real".into(),
        };
        let headers = request_headers(r#"{"Authorization":"fake","X-A":"1"}"#, Some(&secret));
        let headers = headers.unwrap();
        assert_eq!(headers.get_all(AUTHORIZATION).iter().count(), 1);
        assert_eq!(headers[AUTHORIZATION], "Bearer real");
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert_eq!(headers["x-a"], "1");
    }
}
