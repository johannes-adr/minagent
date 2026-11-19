//! Configuration: the settings file `config.json` (edited on the web UI's settings page),
//! the process environment and `.env`, in that order of priority.
//!
//! [`VARS`] is the single source of truth: it drives [`Config::load`], the settings page
//! ([`Config::entries`]) and the generated `.env.example` ([`env_example`]). Values from `.env`
//! and `config.json` are not put into the process environment, so child processes don't
//! inherit them.

#[cfg(feature = "javascript")]
use crate::tools::execute_javascript::{Secret, Settings};
use rig_core::DynModel;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::{self, AnthropicConfig};
use rig_core::providers::ollama::{self, OllamaConfig};
use rig_core::providers::openai::{self, OpenAIConfig};
use rig_core::serde_json::{self, Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The settings file, in the working directory unless `AGENT_CONFIG` names another. A flat
/// JSON object with the names of [`VARS`] as keys, e.g. `{"LLM_BACKEND": "ollama"}`.
pub const FILE: &str = "config.json";

pub fn config_path() -> PathBuf {
    std::env::var_os("AGENT_CONFIG").map_or_else(|| FILE.into(), PathBuf::from)
}

/// Prefix of the variables that define `execute_javascript` secrets.
#[cfg(feature = "javascript")]
const SECRET_PREFIX: &str = "AGENT_SECRET_";

pub struct Var {
    pub name: &'static str,
    /// How the settings page edits it: `text`, `number`, `bool`, `list` (comma separated),
    /// `tools`, `skills`, `choice:<a>,<b>`, `secret` (never shown again once saved) or `secret-host`
    /// (`<host> <value>`: only the host is shown again).
    pub kind: &'static str,
    pub example: &'static str,
    pub doc: &'static str,
}

pub const VARS: &[Var] = &[
    Var {
        name: "LLM_BACKEND",
        kind: "choice:ollama,anthropic,openai",
        example: "openai",
        doc: "LLM backend: ollama, anthropic or openai",
    },
    Var {
        name: "LLM_MODEL",
        kind: "text",
        example: "",
        doc: "Model name; empty uses the backend's default",
    },
    Var {
        name: "AGENT_CONTEXT_WINDOW",
        kind: "number",
        example: "",
        doc: "Context window of the model in tokens, for the context display in the UIs; empty uses rig's model catalog. Set it for Ollama models to the daemon's context length (num_ctx)",
    },
    Var {
        name: "OPENAI_API_KEY",
        kind: "secret",
        example: "",
        doc: "OpenAI API key (required for the openai backend)",
    },
    Var {
        name: "OPENAI_BASE_URL",
        kind: "text",
        example: "",
        doc: "OpenAI-compatible API base URL; empty uses the official API",
    },
    Var {
        name: "ANTHROPIC_API_KEY",
        kind: "secret",
        example: "",
        doc: "Anthropic API key (required for the anthropic backend)",
    },
    Var {
        name: "ANTHROPIC_BASE_URL",
        kind: "text",
        example: "",
        doc: "Anthropic API base URL; empty uses the official API",
    },
    Var {
        name: "OLLAMA_API_BASE_URL",
        kind: "text",
        example: "",
        doc: "Ollama daemon URL; empty uses http://localhost:11434",
    },
    Var {
        name: "OLLAMA_API_KEY",
        kind: "secret",
        example: "",
        doc: "Ollama token, only for authenticated/proxied daemons",
    },
    Var {
        name: "AGENT_TOOLS",
        kind: "tools",
        example: "",
        doc: "Comma-separated tools to enable (execute, use_skill, agent, ask_user, execute_javascript, view_image); empty enables all",
    },
    Var {
        name: "AGENT_DISABLED_SKILLS",
        kind: "skills",
        example: "",
        doc: "Comma-separated skills to switch off by default (a chat can change that); empty keeps all skills on",
    },
    Var {
        name: "AGENT_JS_ALLOWED_HOSTS",
        kind: "list",
        example: "",
        doc: "execute_javascript (unless built with --no-default-features): comma-separated hosts its scripts may contact without asking",
    },
    Var {
        name: "AGENT_JS_ALLOW_WRITES",
        kind: "bool",
        example: "",
        doc: "execute_javascript: set to 1 to let scripts also change data (POST, PUT, PATCH, DELETE, uploads) on the allowed hosts without asking; otherwise each such request asks for approval while the script runs",
    },
    Var {
        name: "AGENT_SECRET_GITHUB",
        kind: "secret-host",
        example: "",
        doc: "execute_javascript: a secret named GITHUB as `<host> <Authorization header value>`, e.g. `api.github.com Bearer ghp_xxx`; scripts use it with fetch(..., {secret: \"GITHUB\"}) and never see the value. Add more as AGENT_SECRET_<NAME>",
    },
    Var {
        name: "AGENT_SECRET_PAPERLESS",
        kind: "secret-host",
        example: "",
        doc: "Built-in skill `paperless`: the Paperless-ngx API token (web UI: My Profile) as `<host> Token <token>`, e.g. `paperless.example.com Token 0123abc`. Add the host to AGENT_JS_ALLOWED_HOSTS to skip approvals",
    },
];

/// The `.env.example` contents generated from [`VARS`].
pub fn env_example() -> String {
    let mut out = String::from("# Generated by `agent --env-example`; copy to .env and fill in.\n");
    for var in VARS {
        out += &format!("\n# {}\n{}={}\n", var.doc, var.name, var.example);
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Backend {
    Ollama,
    Anthropic,
    OpenAI,
}

#[derive(Clone)]
pub struct Config {
    values: HashMap<String, String>,
    /// The names the settings file sets, and those the environment (or `.env`) sets.
    from_file: HashSet<String>,
    from_env: HashSet<String>,
}

/// Whether `name` may be set in the settings file.
fn known(name: &str) -> bool {
    VARS.iter().any(|var| var.name == name) || is_secret_name(name)
}

#[cfg(feature = "javascript")]
fn is_secret_name(name: &str) -> bool {
    name.strip_prefix(SECRET_PREFIX).is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}
#[cfg(not(feature = "javascript"))]
fn is_secret_name(_name: &str) -> bool {
    false
}

/// The settings file's entries; a missing file has none.
fn read_file(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let object: Map<String, Value> = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} must be a JSON object: {e}", path.display()))?;
    let mut entries = BTreeMap::new();
    for (name, value) in object {
        let text = match value {
            Value::String(text) => text,
            Value::Bool(_) | Value::Number(_) => value.to_string(),
            Value::Null => continue,
            _ => return Err(format!("{}: {name} must be a string", path.display())),
        };
        if known(&name) {
            entries.insert(name, text);
        }
    }
    Ok(entries)
}

/// Writes the settings file; a failure leaves the old one intact.
fn write_file(path: &Path, entries: &BTreeMap<String, String>) -> Result<(), String> {
    let mut json = serde_json::to_vec_pretty(entries).map_err(|e| e.to_string())?;
    json.push(b'\n');
    write_private(path, &json)
}

/// Writes `bytes` to `path` via a temporary file, so that a failure leaves the old file
/// intact. The files hold keys and tokens, so only the owner may read them.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let fail = |e: std::io::Error| format!("could not write {}: {e}", path.display());
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp).map_err(fail)?;
    file.write_all(bytes).map_err(fail)?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(fail)
}

impl Config {
    /// The settings file first, then the process environment, then `.env` in the current
    /// directory (if present).
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::load_from(&config_path())?)
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        Self::layered(read_file(path)?)
    }

    fn layered(file: BTreeMap<String, String>) -> Result<Self, String> {
        let mut values = HashMap::new();
        if Path::new(".env").exists() {
            let items = dotenvy::from_path_iter(".env").map_err(|e| e.to_string())?;
            for item in items {
                let (key, value) = item.map_err(|e| e.to_string())?;
                values.insert(key, value);
            }
        }
        let mut from_env = HashSet::new();
        for var in VARS {
            if let Ok(value) = std::env::var(var.name) {
                values.insert(var.name.to_owned(), value);
            }
        }
        // Secrets have free names, so they are not in `VARS`.
        #[cfg(feature = "javascript")]
        values.extend(std::env::vars().filter(|(name, _)| name.starts_with(SECRET_PREFIX)));
        from_env.extend(values.keys().cloned());
        let from_file = file.keys().cloned().collect();
        values.extend(file);
        Ok(Config {
            values,
            from_file,
            from_env,
        })
    }

    /// Applies `changes` to the settings file: a string sets that value, `null` removes it
    /// (the environment's value, if any, applies again). Nothing is written if the result
    /// is not a valid configuration. Returns the new configuration.
    pub fn save_changes(path: &Path, changes: &Map<String, Value>) -> Result<Self, String> {
        let mut file = read_file(path)?;
        for (name, value) in changes {
            if !known(name) {
                return Err(format!("unknown setting {name}"));
            }
            match value {
                Value::Null => {
                    file.remove(name);
                }
                Value::String(text) => {
                    file.insert(name.clone(), text.clone());
                }
                _ => return Err(format!("{name} must be a string or null")),
            }
        }
        let config = Self::layered(file.clone())?;
        config.validate()?;
        write_file(path, &file)?;
        Ok(config)
    }

    /// Checks everything except the API key, which the settings page may still be missing.
    pub fn validate(&self) -> Result<(), String> {
        self.backend()?;
        self.context_window()?;
        if let Some(tools) = self.tools() {
            crate::harness::validate_tools(&tools).map_err(|e| format!("AGENT_TOOLS: {e}"))?;
        }
        #[cfg(feature = "javascript")]
        self.javascript()?;
        Ok(())
    }

    /// What is wrong with the configuration as it stands, for the settings page.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let Err(e) = self.validate() {
            problems.push(e);
        } else if let Err(e) = self.model() {
            problems.push(e);
        }
        problems
    }

    /// Every setting for the settings page. Secrets are never sent back: `set` says whether
    /// they have a value, and `hint` is the host of a `secret-host`.
    pub fn entries(&self) -> Vec<Value> {
        let mut secrets: Vec<&String> = self
            .values
            .keys()
            .filter(|name| is_secret_name(name) && !VARS.iter().any(|v| v.name == name.as_str()))
            .collect();
        secrets.sort();
        let custom = secrets.into_iter().map(|name| {
            (
                name.as_str(),
                "execute_javascript secret as `<host> <Authorization header value>`",
                "secret-host",
                true,
            )
        });
        let known = VARS.iter().map(|v| (v.name, v.doc, v.kind, false));
        known
            .chain(custom)
            .map(|(name, doc, kind, custom)| {
                let value = self.get(name);
                let source = if self.from_file.contains(name) {
                    "file"
                } else if self.from_env.contains(name) {
                    "env"
                } else {
                    "default"
                };
                let secret = kind.starts_with("secret");
                json!({
                    "name": name,
                    "doc": doc,
                    "kind": kind,
                    "custom": custom,
                    "source": source,
                    "set": value.is_some(),
                    "value": if secret { "" } else { value.unwrap_or_default() },
                    "hint": if kind == "secret-host" {
                        value.and_then(|v| v.split_whitespace().next())
                    } else {
                        None
                    },
                })
            })
            .collect()
    }

    /// The value of `name`; empty counts as unset.
    fn get(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn required(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("{name} is not set (in the environment or .env)"))
    }

    /// The tools listed in `AGENT_TOOLS` (comma separated); `None` means all tools.
    pub fn tools(&self) -> Option<Vec<String>> {
        let list: Vec<String> = self
            .get("AGENT_TOOLS")?
            .split(',')
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect();
        (!list.is_empty()).then_some(list)
    }

    /// The skills switched off in `AGENT_DISABLED_SKILLS` (comma separated).
    pub fn disabled_skills(&self) -> Vec<String> {
        self.get("AGENT_DISABLED_SKILLS")
            .unwrap_or_default()
            .split(',')
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// The context window set in `AGENT_CONTEXT_WINDOW`; `None` leaves it to the model catalog.
    pub fn context_window(&self) -> Result<Option<u64>, String> {
        let Some(value) = self.get("AGENT_CONTEXT_WINDOW") else {
            return Ok(None);
        };
        match value.trim().replace('_', "").parse::<u64>() {
            Ok(tokens) if tokens > 0 => Ok(Some(tokens)),
            _ => Err(format!(
                "AGENT_CONTEXT_WINDOW must be a number of tokens, e.g. 32768, not {value:?}"
            )),
        }
    }

    /// A comma-separated list of hosts, trimmed and lowercase.
    #[cfg(feature = "javascript")]
    fn hosts(&self, name: &str) -> Vec<String> {
        self.get(name)
            .unwrap_or_default()
            .split(',')
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect()
    }

    /// The configuration of `execute_javascript`: allowed hosts and secrets
    /// (`AGENT_SECRET_<NAME>=<host> <header value>`).
    #[cfg(feature = "javascript")]
    pub fn javascript(&self) -> Result<Settings, String> {
        let mut secrets = HashMap::new();
        for (var, value) in &self.values {
            let Some(name) = var.strip_prefix(SECRET_PREFIX).filter(|n| !n.is_empty()) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            let (host, header) = value
                .split_once(char::is_whitespace)
                .filter(|(_, header)| !header.trim().is_empty())
                .ok_or_else(|| format!("{var} must be `<host> <header value>`"))?;
            let secret = Secret {
                host: host.to_ascii_lowercase(),
                value: header.trim().to_owned(),
            };
            secrets.insert(name.to_owned(), secret);
        }
        Ok(Settings {
            allowed_hosts: self.hosts("AGENT_JS_ALLOWED_HOSTS"),
            allow_writes: matches!(
                self.get("AGENT_JS_ALLOW_WRITES")
                    .map(|v| v.trim().to_ascii_lowercase())
                    .as_deref(),
                Some("1" | "true" | "yes")
            ),
            secrets,
        })
    }

    pub fn backend(&self) -> Result<Backend, String> {
        match self.get("LLM_BACKEND").unwrap_or("openai") {
            "ollama" => Ok(Backend::Ollama),
            "anthropic" => Ok(Backend::Anthropic),
            "openai" => Ok(Backend::OpenAI),
            other => Err(format!(
                "unknown LLM_BACKEND {other:?}; use ollama, anthropic or openai"
            )),
        }
    }

    /// Address, token and model name of the Ollama daemon, if that is the backend.
    pub fn ollama(&self) -> Option<(String, Option<String>, String)> {
        if self.backend() != Ok(Backend::Ollama) {
            return None;
        }
        Some((
            self.get("OLLAMA_API_BASE_URL")
                .unwrap_or("http://localhost:11434")
                .to_owned(),
            self.get("OLLAMA_API_KEY").map(str::to_owned),
            self.get("LLM_MODEL").unwrap_or(ollama::QWEN3).to_owned(),
        ))
    }

    /// The completion model for the configured backend; fails without its API key.
    pub fn model(&self) -> Result<DynModel<Completion>, String> {
        match self.backend()? {
            Backend::OpenAI => self.required("OPENAI_API_KEY")?,
            Backend::Anthropic => self.required("ANTHROPIC_API_KEY")?,
            Backend::Ollama => "",
        };
        self.model_unchecked()
    }

    /// Like [`Config::model`], but with an empty API key where one is missing, so that the
    /// web UI can start and the key can be entered on its settings page. Requests to the
    /// provider then fail with its own error.
    pub fn model_unchecked(&self) -> Result<DynModel<Completion>, String> {
        let backend = self.backend()?;
        let model = |default: &str| self.get("LLM_MODEL").unwrap_or(default).to_owned();
        let key = |name: &str| self.get(name).unwrap_or_default().to_owned();
        Ok(match backend {
            Backend::OpenAI => {
                let mut c = OpenAIConfig::new(key("OPENAI_API_KEY"));
                if let Some(url) = self.get("OPENAI_BASE_URL") {
                    c = c.with_base_url(url);
                }
                c.client().completion(model(openai::GPT_5_5)).into()
            }
            Backend::Anthropic => {
                let mut c = AnthropicConfig::new(key("ANTHROPIC_API_KEY"));
                if let Some(url) = self.get("ANTHROPIC_BASE_URL") {
                    c = c.with_base_url(url);
                }
                c.client()
                    .completion(model(anthropic::CLAUDE_SONNET_5_5))
                    .into()
            }
            Backend::Ollama => {
                let mut c = OllamaConfig::new();
                if let Some(url) = self.get("OLLAMA_API_BASE_URL") {
                    c = c.with_base_url(url);
                }
                if let Some(key) = self.get("OLLAMA_API_KEY") {
                    c = c.with_api_key(key);
                }
                c.client().completion(model(ollama::QWEN3)).into()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pairs: &[(&str, &str)]) -> Config {
        Config {
            values: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            from_file: HashSet::new(),
            from_env: HashSet::new(),
        }
    }

    #[test]
    fn env_example_is_up_to_date() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env.example");
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed,
            env_example(),
            ".env.example is stale; run `cargo run -- --env-example`"
        );
    }

    #[test]
    fn env_example_lists_every_var() {
        let example = env_example();
        for var in VARS {
            assert!(example.contains(&format!("\n{}=", var.name)), "{example}");
        }
    }

    #[test]
    fn disabled_skills_list() {
        assert!(config(&[]).disabled_skills().is_empty());
        assert_eq!(
            config(&[("AGENT_DISABLED_SKILLS", " paperless, ,other ")]).disabled_skills(),
            ["paperless", "other"]
        );
    }

    #[test]
    fn tool_list() {
        assert_eq!(config(&[]).tools(), None);
        assert_eq!(config(&[("AGENT_TOOLS", " , ")]).tools(), None);
        assert_eq!(
            config(&[("AGENT_TOOLS", "execute, agent")]).tools(),
            Some(vec!["execute".to_owned(), "agent".to_owned()])
        );
    }

    #[cfg(feature = "javascript")]
    #[test]
    fn javascript_settings() {
        let settings = config(&[
            ("AGENT_JS_ALLOWED_HOSTS", " API.github.com, ,example.com "),
            ("AGENT_SECRET_GITHUB", "api.github.com Bearer ghp_x"),
            ("AGENT_SECRET_EMPTY", ""),
            ("AGENT_SECRET_", "ignored"),
        ])
        .javascript()
        .unwrap();
        assert_eq!(settings.allowed_hosts, ["api.github.com", "example.com"]);
        assert_eq!(settings.secrets.len(), 1);
        assert_eq!(settings.secrets["GITHUB"].host, "api.github.com");
        assert_eq!(settings.secrets["GITHUB"].value, "Bearer ghp_x");
        assert!(config(&[]).javascript().unwrap().allowed_hosts.is_empty());
        assert!(!settings.allow_writes);
        let writes = config(&[("AGENT_JS_ALLOW_WRITES", "1")])
            .javascript()
            .unwrap();
        assert!(writes.allow_writes);
    }

    #[cfg(feature = "javascript")]
    #[test]
    fn malformed_secret_is_an_error_without_the_value() {
        for value in ["api.github.com", "api.github.com   "] {
            let err = config(&[("AGENT_SECRET_X", value)])
                .javascript()
                .err()
                .unwrap();
            assert_eq!(err, "AGENT_SECRET_X must be `<host> <header value>`");
        }
    }

    #[test]
    fn backend_selection() {
        assert_eq!(config(&[]).backend(), Ok(Backend::OpenAI));
        assert_eq!(
            config(&[("LLM_BACKEND", "")]).backend(),
            Ok(Backend::OpenAI)
        );
        assert_eq!(
            config(&[("LLM_BACKEND", "ollama")]).backend(),
            Ok(Backend::Ollama)
        );
        assert_eq!(
            config(&[("LLM_BACKEND", "anthropic")]).backend(),
            Ok(Backend::Anthropic)
        );
        assert!(config(&[("LLM_BACKEND", "gemini")]).backend().is_err());
    }

    #[test]
    fn missing_api_key_is_an_error() {
        let err = config(&[("LLM_BACKEND", "anthropic")])
            .model()
            .err()
            .unwrap();
        assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
        let err = config(&[("OPENAI_API_KEY", "")]).model().err().unwrap();
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }

    #[test]
    fn context_window_override() {
        assert_eq!(config(&[]).context_window(), Ok(None));
        assert_eq!(
            config(&[("AGENT_CONTEXT_WINDOW", " 32_768 ")]).context_window(),
            Ok(Some(32_768))
        );
        for bad in ["0", "32k", "-1"] {
            let err = config(&[("AGENT_CONTEXT_WINDOW", bad)])
                .context_window()
                .err()
                .unwrap();
            assert!(err.contains("AGENT_CONTEXT_WINDOW"), "{err}");
        }
    }

    #[test]
    fn ollama_needs_no_key() {
        assert!(config(&[("LLM_BACKEND", "ollama")]).model().is_ok());
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_config_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    fn entry(config: &Config, name: &str) -> Value {
        config.entries().into_iter().find(|e| e["name"] == name).unwrap()
    }

    #[test]
    fn the_settings_file_wins_and_can_be_changed() {
        let path = temp_file("changes");
        assert!(Config::load_from(&path).is_ok()); // no file yet
        let changes = json!({"LLM_MODEL": "m1", "AGENT_JS_ALLOW_WRITES": "1"});
        let config = Config::save_changes(&path, changes.as_object().unwrap()).unwrap();
        assert_eq!(config.get("LLM_MODEL"), Some("m1"));
        assert_eq!(entry(&config, "LLM_MODEL")["source"], "file");
        // The file holds the change, and a fresh load sees it.
        assert_eq!(Config::load_from(&path).unwrap().get("LLM_MODEL"), Some("m1"));
        // `null` removes a setting from the file.
        let remove = json!({"LLM_MODEL": null});
        let config = Config::save_changes(&path, remove.as_object().unwrap()).unwrap();
        assert_ne!(entry(&config, "LLM_MODEL")["source"], "file");
        assert_eq!(entry(&config, "AGENT_JS_ALLOW_WRITES")["value"], "1");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn invalid_changes_are_rejected_and_not_saved() {
        let path = temp_file("invalid");
        for bad in [
            json!({"LLM_BACKEND": "gemini"}),
            json!({"AGENT_CONTEXT_WINDOW": "lots"}),
            json!({"AGENT_TOOLS": "execute, rm"}),
            json!({"NOT_A_SETTING": "x"}),
            json!({"LLM_MODEL": 5}),
            json!({"AGENT_SECRET_X": "no-header"}),
        ] {
            assert!(Config::save_changes(&path, bad.as_object().unwrap()).is_err(), "{bad}");
        }
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn secrets_are_never_listed_with_their_value() {
        let path = temp_file("secrets");
        let changes = json!({
            "OPENAI_API_KEY": "sk-secret",
            "AGENT_SECRET_GITHUB": "api.github.com Bearer ghp_secret",
            "AGENT_SECRET_OTHER": "example.com Token t0ps3cret",
        });
        let config = Config::save_changes(&path, changes.as_object().unwrap()).unwrap();
        let listed = Value::Array(config.entries()).to_string();
        for secret in ["sk-secret", "ghp_secret", "t0ps3cret"] {
            assert!(!listed.contains(secret), "{listed}");
        }
        let key = entry(&config, "OPENAI_API_KEY");
        assert_eq!((&key["set"], &key["value"]), (&json!(true), &json!("")));
        // Only the host of a secret is shown; free-named secrets are listed too.
        assert_eq!(entry(&config, "AGENT_SECRET_GITHUB")["hint"], "api.github.com");
        let other = entry(&config, "AGENT_SECRET_OTHER");
        assert_eq!((&other["custom"], &other["hint"]), (&json!(true), &json!("example.com")));
        // The file is readable by its owner only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_key_is_a_problem_but_not_an_invalid_config() {
        let config = config(&[("LLM_BACKEND", "anthropic")]);
        assert!(config.validate().is_ok());
        assert_eq!(config.problems().len(), 1);
        assert!(config.model_unchecked().is_ok());
    }
}
