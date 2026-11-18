//! `fetch` of `execute_javascript`: HTTP requests to the hosts the call declared.

use super::{Secret, Settings, fs};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use reqwest::multipart::{Form, Part};
use reqwest::{Method, Url, redirect};
use rig_core::serde_json::{self, Map, Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

/// Responses larger than this fail the request.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
/// Limit for responses saved to a file (`saveTo`) and for files sent in a `form`.
const MAX_FILE_BYTES: usize = 100 * 1024 * 1024;
/// The capability behind `fetch` for one script run.
pub(super) struct Fetch {
    /// Normalized hostnames the script may contact (the `hosts` argument).
    pub hosts: Vec<String>,
    pub settings: Arc<Settings>,
    /// The end of the run; requests don't outlive it.
    pub deadline: Instant,
    pub handle: Handle,
}

/// The options of a `fetch` call, as `prelude.js` passes them (JSON).
#[derive(Debug, Default, PartialEq)]
struct Options {
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    secret: Option<String>,
    /// `form`: multipart/form-data fields in order; a name may repeat.
    form: Option<Vec<(String, Field)>>,
    /// `saveTo`: file in the working directory that receives a successful response body.
    save_to: Option<String>,
}

/// A value of `form`.
#[derive(Debug, PartialEq)]
enum Field {
    Text(String),
    /// `{ file, filename?, type? }`: the bytes of a file in the working directory.
    File {
        path: String,
        filename: Option<String>,
        mime: Option<String>,
    },
}

fn string(value: Option<&Value>, what: &str) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{what}` must be a string")),
    }
}

impl Options {
    fn parse(json: &str) -> Result<Options, String> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| format!("invalid fetch options: {e}"))?;
        let Value::Object(options) = value else {
            return Err("fetch options must be an object".into());
        };
        let mut headers = Vec::new();
        if let Some(Value::Object(given)) = options.get("headers") {
            for (name, value) in given {
                let value = value.as_str().ok_or("header values must be strings")?;
                headers.push((name.clone(), value.to_owned()));
            }
        }
        let form = match options.get("form") {
            None | Some(Value::Null) => None,
            Some(Value::Object(fields)) => Some(parse_form(fields)?),
            Some(_) => return Err("`form` must be an object".into()),
        };
        let body = string(options.get("body"), "body")?;
        if body.is_some() && form.is_some() {
            return Err("pass either `body` or `form`, not both".into());
        }
        Ok(Options {
            method: string(options.get("method"), "method")?.unwrap_or_else(|| "GET".into()),
            headers,
            body,
            secret: string(options.get("secret"), "secret")?,
            form,
            save_to: string(options.get("saveTo"), "saveTo")?,
        })
    }
}

/// `{name: value}` where a value is a string, number or boolean, a `{file, filename?, type?}`
/// object, or an array of those (the field is repeated); `null` skips the field.
fn parse_form(fields: &Map<String, Value>) -> Result<Vec<(String, Field)>, String> {
    let mut out = Vec::new();
    for (name, value) in fields {
        let values = match value {
            Value::Array(values) => values.iter().collect(),
            value => vec![value],
        };
        for value in values {
            let field = match value {
                Value::Null => continue,
                Value::String(s) => Field::Text(s.clone()),
                Value::Number(_) | Value::Bool(_) => Field::Text(value.to_string()),
                Value::Object(file) => Field::File {
                    path: string(file.get("file"), "form file")?
                        .ok_or_else(|| format!("form field {name:?}: expected {{ file: path }}"))?,
                    filename: string(file.get("filename"), "filename")?,
                    mime: string(file.get("type"), "type")?,
                },
                Value::Array(_) => return Err(format!("form field {name:?}: nested array")),
            };
            out.push((name.clone(), field));
        }
    }
    Ok(out)
}

/// The request body.
enum Payload {
    None,
    Text(String),
    Form(Form),
}

/// Builds the multipart form, reading files from the working directory.
fn build_form(fields: Vec<(String, Field)>) -> Result<Form, String> {
    let mut form = Form::new();
    for (name, field) in fields {
        form = match field {
            Field::Text(text) => form.text(name, text),
            Field::File {
                path,
                filename,
                mime,
            } => {
                let bytes = fs::read_bytes(&path, MAX_FILE_BYTES as u64)?;
                let filename =
                    filename.unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path).to_owned());
                let mime = mime.unwrap_or_else(|| {
                    mime_guess::from_path(&filename)
                        .first_or_octet_stream()
                        .to_string()
                });
                let part = Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(&mime)
                    .map_err(|_| format!("invalid type {mime:?}"))?;
                form.part(name, part)
            }
        };
    }
    Ok(form)
}

impl Fetch {
    /// Checks that `url` may be requested and that `secret` (if any) may be sent to it.
    fn authorize(&self, url: &str, secret: Option<&str>) -> Result<(Url, Option<&Secret>), String> {
        let url = Url::parse(url).map_err(|e| format!("invalid URL {url:?}: {e}"))?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        match url.scheme() {
            "https" | "http" => {}
            scheme => {
                return Err(format!(
                    "{scheme}:// is not allowed; use https:// or http://"
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

    /// Performs the request described by `options` (JSON) and returns
    /// `{status, headers, body, saved?}` as JSON. Blocks the calling (non-async) thread until
    /// the response is complete.
    pub fn call(&self, url: &str, options: &str) -> Result<String, String> {
        let options = Options::parse(options)?;
        let (url, secret) = self.authorize(url, options.secret.as_deref())?;
        let method = Method::from_bytes(options.method.to_ascii_uppercase().as_bytes())
            .map_err(|_| format!("invalid method {:?}", options.method))?;
        let headers = request_headers(&options.headers, secret)?;
        // Check the target before sending, so a bad path does not cost a request.
        let save_to = options
            .save_to
            .map(|path| fs::writable(&path).map(|file| (file, path)))
            .transpose()?;
        let payload = match (options.body, options.form) {
            (Some(body), _) => Payload::Text(body),
            (None, Some(form)) => Payload::Form(build_form(form)?),
            (None, None) => Payload::None,
        };
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        self.handle
            .block_on(send(url, method, headers, payload, save_to, remaining))
    }
}

/// The headers of the script plus the secret's `Authorization`, which replaces any the
/// script set.
fn request_headers(
    given: &[(String, String)],
    secret: Option<&Secret>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in given {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid header name {name:?}"))?;
        let value =
            HeaderValue::from_str(value).map_err(|_| format!("invalid value for header {name}"))?;
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
    payload: Payload,
    save_to: Option<(PathBuf, String)>,
    timeout: Duration,
) -> Result<String, String> {
    // No redirects: they could lead to a host the script didn't declare.
    let client = reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.request(method, url).headers(headers);
    request = match payload {
        Payload::None => request,
        Payload::Text(body) => request.body(body),
        Payload::Form(form) => request.multipart(form),
    };
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
    // Only a successful body goes to the file; an error body is returned to read it.
    let save_to = save_to.filter(|_| response.status().is_success());
    let limit = if save_to.is_some() {
        MAX_FILE_BYTES
    } else {
        MAX_RESPONSE_BYTES
    };
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > limit {
            return Err(format!("response is larger than {} MB", limit >> 20));
        }
        body.extend_from_slice(&chunk);
    }
    let mut result = json!({ "status": status, "headers": response_headers });
    if let Some((file, path)) = save_to {
        fs::write_resolved(&file, &path, &body)?;
        result["body"] = Value::String(String::new());
        result["saved"] = json!(body.len());
    } else {
        result["body"] = Value::String(String::from_utf8_lossy(&body).into_owned());
    }
    Ok(result.to_string())
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
                secrets,
                ..Settings::default()
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
    fn only_declared_hosts_over_http_or_https() {
        let f = fetch(&["api.example.com", "localhost", "192.168.178.178"], &[]);
        assert!(f.authorize("https://api.example.com/x", None).is_ok());
        assert!(f.authorize("https://API.example.com/x", None).is_ok());
        assert!(f.authorize("http://api.example.com/x", None).is_ok());
        assert!(f.authorize("http://localhost:8080/x", None).is_ok());
        assert!(
            f.authorize("http://192.168.178.178:8004/api/", None)
                .is_ok()
        );
        let err = |url| f.authorize(url, None).err().unwrap();
        assert!(err("https://evil.com/").contains("not declared"));
        assert!(err("http://192.168.178.1/").contains("not declared"));
        assert!(err("https://api.example.com.evil.com/").contains("not declared"));
        assert!(err("https://api.example.com@evil.com/").contains("not declared"));
        assert!(err("file:///etc/passwd").contains("not allowed"));
        assert!(err("ftp://api.example.com/").contains("not allowed"));
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
        let given = [("Authorization", "fake"), ("X-A", "1")].map(|(n, v)| (n.into(), v.into()));
        let headers = request_headers(&given, Some(&secret)).unwrap();
        assert_eq!(headers.get_all(AUTHORIZATION).iter().count(), 1);
        assert_eq!(headers[AUTHORIZATION], "Bearer real");
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert_eq!(headers["x-a"], "1");
    }

    #[test]
    fn options_are_parsed() {
        assert_eq!(
            Options::parse("{}").unwrap(),
            Options {
                method: "GET".into(),
                ..Options::default()
            }
        );
        let options = Options::parse(
            r#"{"method":"post","headers":{"X-A":"1"},"secret":"S","saveTo":"out/a.pdf",
                "form":{"title":"T","tags":[1,2],"skip":null,"flag":true,
                        "document":{"file":"in/a.pdf","filename":"b.pdf","type":"application/pdf"}}}"#,
        )
        .unwrap();
        let text = |name: &str, value: &str| (name.to_owned(), Field::Text(value.to_owned()));
        assert_eq!(
            options,
            Options {
                method: "post".into(),
                headers: vec![("X-A".into(), "1".into())],
                body: None,
                secret: Some("S".into()),
                form: Some(vec![
                    (
                        "document".into(),
                        Field::File {
                            path: "in/a.pdf".into(),
                            filename: Some("b.pdf".into()),
                            mime: Some("application/pdf".into()),
                        }
                    ),
                    text("flag", "true"),
                    text("tags", "1"),
                    text("tags", "2"),
                    text("title", "T"),
                ]),
                save_to: Some("out/a.pdf".into()),
            }
        );
        let err = |json| Options::parse(json).err().unwrap();
        assert!(err(r#"{"body":"x","form":{}}"#).contains("either `body` or `form`"));
        assert!(err(r#"{"form":{"f":{"name":"x"}}}"#).contains("expected { file: path }"));
        assert!(err(r#"{"form":{"f":[[1]]}}"#).contains("nested array"));
        assert!(err(r#"{"saveTo":1}"#).contains("`saveTo` must be a string"));
        assert!(err("[]").contains("must be an object"));
    }
}
