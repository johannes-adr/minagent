//! Webhooks: an outside service can start the agent with `POST /hooks/<name>`.
//!
//! A hook has its own token (stored only as a hash), a prompt template filled from the
//! request, and what its runs may do. Nobody is there to answer questions during a run, so
//! everything that would ask the user is declined unless the hook allows it: only the listed
//! tools exist, JavaScript may reach only the listed hosts, and `execute` (a shell) is only
//! available inside an isolated container. Hooks live in `hooks.json` next to the settings
//! file; see `frontend::hook_run` for how runs are queued and executed.

use crate::config::write_private;
use crate::harness::{self, Toolset};
use rig_core::serde_json::{self, Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const FILE: &str = "hooks.json";
/// Waiting runs; more are refused so that a service in an error loop can't flood the agent.
pub const MAX_QUEUE: usize = 50;
/// The payload shown to the model through `{{payload}}` is cut to this many characters.
const MAX_PAYLOAD: usize = 50_000;
const MAX_TEMPLATE: usize = 20_000;
const DEFAULT_TIMEOUT_MINUTES: u64 = 10;

/// The hooks file next to the settings file.
pub fn path_for(config: &Path) -> PathBuf {
    config.with_file_name(FILE)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hook {
    /// The name in the URL: `/hooks/<name>`.
    pub name: String,
    /// SHA-256 of the token, hex.
    pub token_hash: String,
    /// The prompt, with `{{payload}}`, `{{body.a.b}}`, `{{headers.name}}` and `{{hook}}`.
    pub template: String,
    /// The tools a run has.
    pub tools: Vec<String>,
    /// The hosts scripts may reach without asking (instead of `AGENT_JS_ALLOWED_HOSTS`).
    pub hosts: Vec<String>,
    /// Whether scripts may change data (POST, PUT, ...) on those hosts.
    pub allow_writes: bool,
    /// Where to POST the result of a run.
    pub callback_url: Option<String>,
    pub enabled: bool,
    /// A run is stopped after this long.
    pub timeout_minutes: u64,
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Hook {
    /// Whether `token` is this hook's, compared in constant time.
    pub fn check_token(&self, token: &str) -> bool {
        let (a, b) = (hash(token), &self.token_hash);
        a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// What the page may see: everything but the token's hash.
    pub fn to_public(&self) -> Value {
        json!({
            "name": self.name,
            "template": self.template,
            "tools": self.tools,
            "hosts": self.hosts,
            "allow_writes": self.allow_writes,
            "callback_url": self.callback_url,
            "enabled": self.enabled,
            "timeout_minutes": self.timeout_minutes,
        })
    }

    fn to_json(&self) -> Value {
        let mut value = self.to_public();
        value["token_hash"] = json!(self.token_hash);
        value
    }

    fn from_json(value: &Value) -> Option<Self> {
        let strings = |key: &str| -> Vec<String> {
            value[key]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        Some(Self {
            name: value["name"].as_str()?.to_owned(),
            token_hash: value["token_hash"].as_str()?.to_owned(),
            template: value["template"].as_str()?.to_owned(),
            tools: strings("tools"),
            hosts: strings("hosts"),
            allow_writes: value["allow_writes"].as_bool().unwrap_or(false),
            callback_url: value["callback_url"].as_str().map(str::to_owned),
            enabled: value["enabled"].as_bool().unwrap_or(true),
            timeout_minutes: value["timeout_minutes"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MINUTES),
        })
    }

    /// The tools of a run. `ask_user` is never there (nobody answers), and `execute` only
    /// where it can't hurt the machine: in an isolated container.
    pub fn toolset(&self, isolated: bool) -> Toolset {
        let names = self
            .tools
            .iter()
            .filter(|t| t.as_str() != "ask_user" && (isolated || t.as_str() != "execute"))
            .cloned()
            .collect();
        let toolset = Toolset::only(Some(names));
        #[cfg(feature = "javascript")]
        let toolset = toolset.with_js(crate::tools::execute_javascript::settings_with(
            self.hosts.clone(),
            self.allow_writes,
        ));
        toolset
    }

    /// Added to the system prompt of a run.
    pub fn note(&self) -> String {
        format!(
            "This conversation was started by the webhook `{}`, not by a person: nobody can \
             answer questions, and tool calls that would need the user's approval are declined \
             automatically. Complete the task on your own and finish with a short report of the \
             result.",
            self.name
        )
    }
}

/// The settings of a hook as sent by the page, checked. `name` is needed for a new hook;
/// an existing one keeps its name and token.
pub struct Fields {
    pub name: Option<String>,
    pub template: String,
    pub tools: Vec<String>,
    pub hosts: Vec<String>,
    pub allow_writes: bool,
    pub callback_url: Option<String>,
    pub enabled: bool,
    pub timeout_minutes: u64,
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 40
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Fields {
    pub fn parse(body: &Value, isolated: bool) -> Result<Self, String> {
        let strings = |key: &str| -> Result<Vec<String>, String> {
            match &body[key] {
                Value::Null => Ok(Vec::new()),
                Value::Array(items) => items
                    .iter()
                    .map(|v| v.as_str().map(|s| s.trim().to_owned()).ok_or(format!("{key} must be a list of strings")))
                    .collect(),
                _ => Err(format!("{key} must be a list of strings")),
            }
        };
        let name = body["name"].as_str().map(|n| n.trim().to_owned());
        if let Some(name) = &name
            && !valid_name(name)
        {
            return Err("the name may have lowercase letters, digits, - and _ (at most 40)".into());
        }
        let template = body["template"].as_str().unwrap_or_default().trim().to_owned();
        if template.is_empty() {
            return Err("the prompt template is empty".into());
        }
        if template.chars().count() > MAX_TEMPLATE {
            return Err(format!("the prompt template is longer than {MAX_TEMPLATE} characters"));
        }
        let tools = strings("tools")?;
        let known = harness::tool_names();
        for tool in &tools {
            if tool == "ask_user" {
                return Err("ask_user can't be used in a webhook run: nobody can answer".into());
            }
            if !known.contains(&tool.as_str()) {
                return Err(format!("unknown tool {tool:?}"));
            }
            if tool == "execute" && !isolated {
                return Err("execute (a shell) is only available to webhooks inside an isolated container (AGENT_ISOLATED=1)".into());
            }
        }
        let mut hosts: Vec<String> = strings("hosts")?
            .into_iter()
            .map(|h| h.to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if let Some(bad) = hosts.iter().find(|h| h.contains(['/', ' ', '@'])) {
            return Err(format!("{bad:?} is not a host name (no scheme, path or spaces)"));
        }
        hosts.dedup();
        let callback_url = match body["callback_url"].as_str().map(str::trim) {
            None | Some("") => None,
            Some(url) => match reqwest::Url::parse(url) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some() => Some(url.to_owned()),
                _ => return Err("the callback URL must be a http(s) URL".into()),
            },
        };
        Ok(Self {
            name,
            template,
            tools,
            hosts,
            allow_writes: body["allow_writes"].as_bool().unwrap_or(false),
            callback_url,
            enabled: body["enabled"].as_bool().unwrap_or(true),
            timeout_minutes: body["timeout_minutes"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MINUTES).clamp(1, 240),
        })
    }
}

/// All hooks, kept in sync with the file.
pub struct Hooks {
    path: PathBuf,
    hooks: Vec<Hook>,
}

impl Hooks {
    /// A missing file means no hooks.
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let hooks = match std::fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                value["hooks"].as_array().map(|a| a.iter().filter_map(Hook::from_json).collect()).unwrap_or_default()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self { path, hooks })
    }

    fn save(&self) -> Result<(), String> {
        let value = json!({"hooks": self.hooks.iter().map(Hook::to_json).collect::<Vec<_>>()});
        let mut bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        write_private(&self.path, &bytes)
    }

    pub fn list(&self) -> &[Hook] {
        &self.hooks
    }

    pub fn get(&self, name: &str) -> Option<&Hook> {
        self.hooks.iter().find(|h| h.name == name)
    }

    /// Adds a hook with `token` (shown to the user once; only its hash is kept), or changes
    /// the settings of the existing one `name` (the token stays). Returns the hook.
    pub fn upsert(&mut self, fields: Fields, token: impl FnOnce() -> String) -> Result<(Hook, Option<String>), String> {
        let existing = fields.name.as_deref().and_then(|n| self.get(n)).cloned();
        let (name, token_hash, fresh) = match existing {
            Some(hook) => (hook.name, hook.token_hash, None),
            None => {
                let name = fields.name.ok_or("a new hook needs a name")?;
                let token = token();
                (name, hash(&token), Some(token))
            }
        };
        let hook = Hook {
            name,
            token_hash,
            template: fields.template,
            tools: fields.tools,
            hosts: fields.hosts,
            allow_writes: fields.allow_writes,
            callback_url: fields.callback_url,
            enabled: fields.enabled,
            timeout_minutes: fields.timeout_minutes,
        };
        let previous = self.hooks.clone();
        match self.hooks.iter_mut().find(|h| h.name == hook.name) {
            Some(slot) => *slot = hook.clone(),
            None => self.hooks.push(hook.clone()),
        }
        if let Err(e) = self.save() {
            self.hooks = previous;
            return Err(e);
        }
        Ok((hook, fresh))
    }

    /// Gives `name` a new token (the old one stops working) and returns it.
    pub fn renew_token(&mut self, name: &str, token: String) -> Result<String, String> {
        let hook = self.hooks.iter_mut().find(|h| h.name == name).ok_or("no such hook")?;
        let old = std::mem::replace(&mut hook.token_hash, hash(&token));
        if let Err(e) = self.save() {
            self.hooks.iter_mut().find(|h| h.name == name).unwrap().token_hash = old;
            return Err(e);
        }
        Ok(token)
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let previous = self.hooks.clone();
        self.hooks.retain(|h| h.name != name);
        if self.hooks.len() == previous.len() {
            return Err("no such hook".into());
        }
        if let Err(e) = self.save() {
            self.hooks = previous;
            return Err(e);
        }
        Ok(())
    }
}

/// What a template can use from a request.
pub struct Request<'a> {
    pub hook: &'a str,
    /// The body as received.
    pub raw: &'a str,
    /// The body as JSON, `Null` if it isn't.
    pub body: &'a Value,
    /// Header names (lowercase) and values.
    pub headers: &'a [(String, String)],
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn lookup(value: &Value, path: &str) -> String {
    let mut current = value;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        let next = match current {
            Value::Object(map) => map.get(part),
            Value::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        };
        match next {
            Some(next) => current = next,
            None => return String::new(),
        }
    }
    text(current)
}

/// Fills `{{...}}` in `template`: `payload` (the raw body), `body.a.b.0` (a field of the JSON
/// body), `headers.x-github-event` and `hook`. Anything missing becomes empty text.
pub fn render(template: &str, request: &Request) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("}}") else {
            rest = &rest[start..];
            break;
        };
        let expr = rest[start + 2..start + end].trim();
        out.push_str(&match expr {
            "payload" => request.raw.chars().take(MAX_PAYLOAD).collect(),
            "hook" => request.hook.to_owned(),
            "body" => text(request.body),
            _ => match expr.split_once('.') {
                Some(("body", path)) => lookup(request.body, path),
                Some(("headers", name)) => request
                    .headers
                    .iter()
                    .find(|(n, _)| *n == name.to_ascii_lowercase())
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
                _ => String::new(),
            },
        });
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_hooks_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    fn fields(extra: Value) -> Result<Fields, String> {
        let mut body = json!({"name": "gh", "template": "Issue: {{body.issue.title}}"});
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        Fields::parse(&body, false)
    }

    #[test]
    fn templates_are_filled_from_the_request() {
        let body = json!({"issue": {"title": "Crash", "labels": ["bug", "p1"], "n": 7}, "x": null});
        let raw = body.to_string();
        let headers = vec![("x-github-event".to_owned(), "issues".to_owned())];
        let request = Request { hook: "gh", raw: &raw, body: &body, headers: &headers };
        let render = |t: &str| render(t, &request);
        assert_eq!(render("{{ body.issue.title }} / {{body.issue.labels.1}} / {{body.issue.n}}"), "Crash / p1 / 7");
        assert_eq!(render("{{headers.X-GitHub-Event}} on {{hook}}"), "issues on gh");
        assert_eq!(render("{{payload}}"), raw);
        assert_eq!(render("[{{body.nothing.here}}][{{body.x}}][{{unknown}}]"), "[][][]");
        assert_eq!(render("{{body.issue.labels}}"), r#"["bug","p1"]"#);
        assert_eq!(render("no braces, open {{ never closed"), "no braces, open {{ never closed");
    }

    #[test]
    fn long_payloads_are_cut() {
        let raw = "x".repeat(MAX_PAYLOAD + 10);
        let request = Request { hook: "h", raw: &raw, body: &Value::Null, headers: &[] };
        assert_eq!(render("{{payload}}", &request).len(), MAX_PAYLOAD);
    }

    #[test]
    fn fields_are_checked() {
        assert!(fields(json!({})).is_ok());
        for bad in [
            json!({"name": "Has Space"}),
            json!({"name": "UPPER"}),
            json!({"template": "  "}),
            json!({"tools": ["ask_user"]}),
            json!({"tools": ["nope"]}),
            json!({"tools": ["execute"]}), // not isolated
            json!({"hosts": ["https://x.com/path"]}),
            json!({"callback_url": "ftp://x"}),
            json!({"callback_url": "not a url"}),
            json!({"tools": "execute"}),
        ] {
            assert!(fields(bad.clone()).is_err(), "{bad}");
        }
        let ok = Fields::parse(&json!({"name": "a", "template": "t", "tools": ["execute"]}), true).unwrap();
        assert_eq!(ok.tools, ["execute"]);
        let ok = fields(json!({"hosts": [" API.GitHub.com "], "timeout_minutes": 9999})).unwrap();
        assert_eq!((ok.hosts, ok.timeout_minutes), (vec!["api.github.com".to_owned()], 240));
    }

    #[test]
    fn tokens_are_stored_as_hashes_and_checked() {
        let path = temp("tokens");
        let mut hooks = Hooks::load(path.clone()).unwrap();
        let (hook, token) = hooks.upsert(fields(json!({})).unwrap(), || "tok123".into()).unwrap();
        assert_eq!(token.as_deref(), Some("tok123"));
        assert!(hook.check_token("tok123") && !hook.check_token("tok124") && !hook.check_token(""));
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(!file.contains("tok123"), "{file}");
        // Reloaded from disk, the hook still accepts its token; changing it keeps the token.
        let mut hooks = Hooks::load(path.clone()).unwrap();
        assert!(hooks.get("gh").unwrap().check_token("tok123"));
        let (changed, fresh) = hooks.upsert(fields(json!({"template": "new"})).unwrap(), || unreachable!()).unwrap();
        assert!(fresh.is_none() && changed.template == "new" && changed.check_token("tok123"));
        // A new token replaces the old one.
        hooks.renew_token("gh", "other".into()).unwrap();
        assert!(!hooks.get("gh").unwrap().check_token("tok123") && hooks.get("gh").unwrap().check_token("other"));
        assert!(hooks.renew_token("nope", "x".into()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
        hooks.remove("gh").unwrap();
        assert!(hooks.remove("gh").is_err());
        assert!(Hooks::load(path.clone()).unwrap().list().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_run_never_gets_ask_user_and_execute_only_in_a_container() {
        let mut hook = Hooks::load(temp("toolset")).unwrap()
            .upsert(fields(json!({})).unwrap(), || "t".into()).unwrap().0;
        hook.tools = vec!["execute".into(), "ask_user".into(), "use_skill".into()];
        assert_eq!(hook.toolset(false).names(), ["use_skill"]);
        assert_eq!(hook.toolset(true).names(), ["execute", "use_skill"]);
    }
}
