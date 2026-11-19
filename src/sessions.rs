//! Chat sessions stored on disk: `./sessions/<id>.json`, one file per conversation.
//!
//! A file holds what is needed to continue the conversation (`history`, the messages the
//! model sees) and what the web page shows (`log`, its events, and the last `context`).

use rig_core::message::Message;
use rig_core::serde_json::{self, Value, json};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DIR: &str = "sessions";
const TITLE_CHARS: usize = 60;

/// What a conversation uses instead of the configured defaults; `None` means the default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Choices {
    /// The tools it has.
    pub tools: Option<Vec<String>>,
    /// The skills it may not use (skills added later are allowed).
    pub skills_disabled: Option<Vec<String>>,
}

/// A stored conversation.
pub struct Saved {
    pub history: Vec<Message>,
    pub log: Vec<Value>,
    pub context: Option<Value>,
    pub choices: Choices,
    /// The tool call a webhook run is paused at, waiting for the user's decision.
    pub pending: Option<Value>,
}

/// The sessions directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Ids become file names, so only plain characters are allowed.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The session's name: a `title` event if the log has one (webhook runs name themselves
/// after their hook), else the first user message, shortened.
fn title(log: &[Value]) -> String {
    if let Some(named) = log.iter().find(|e| e["type"] == "title").and_then(|e| e["text"].as_str()) {
        return named.to_owned();
    }
    let text = log
        .iter()
        .find(|e| e["type"] == "user")
        .and_then(|e| e["text"].as_str())
        .unwrap_or_default();
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > TITLE_CHARS {
        let cut: String = line.chars().take(TITLE_CHARS).collect();
        format!("{cut}…")
    } else {
        line
    }
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// An id no file uses yet: the current time, e.g. `20261118-153012`-like but sortable.
    pub fn new_id(&self) -> String {
        let base = now();
        let mut id = base.to_string();
        let mut n = 1;
        while self.path(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// Writes the session; a failure leaves the old file intact.
    pub fn save(
        &self,
        id: &str,
        history: &[Message],
        log: &[Value],
        context: Option<&Value>,
        choices: &Choices,
        pending: Option<&Value>,
    ) -> io::Result<()> {
        if !valid_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bad session id",
            ));
        }
        fs::create_dir_all(&self.dir)?;
        let created = self
            .read_value(id)
            .and_then(|v| v["created"].as_u64())
            .unwrap_or_else(now);
        let file = json!({
            "id": id,
            "title": title(log),
            "created": created,
            "updated": now(),
            "history": history,
            "log": log,
            "context": context,
            "tools": choices.tools,
            "skills_disabled": choices.skills_disabled,
            "pending": pending,
        });
        let tmp = self.dir.join(format!(".{id}.json.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(&file)?)?;
        fs::rename(tmp, self.path(id))
    }

    fn read_value(&self, id: &str) -> Option<Value> {
        let bytes = fs::read(self.path(id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn load(&self, id: &str) -> Result<Saved, String> {
        if !valid_id(id) {
            return Err("bad session id".into());
        }
        let bytes = fs::read(self.path(id)).map_err(|e| format!("session {id}: {e}"))?;
        let mut file: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("session {id}: {e}"))?;
        let history = serde_json::from_value(file["history"].take())
            .map_err(|e| format!("session {id}: history: {e}"))?;
        let log = match file["log"].take() {
            Value::Array(log) => log,
            _ => Vec::new(),
        };
        let context = Some(file["context"].take()).filter(|c| !c.is_null());
        let choices = Choices {
            tools: serde_json::from_value(file["tools"].take()).ok(),
            skills_disabled: serde_json::from_value(file["skills_disabled"].take()).ok(),
        };
        let pending = Some(file["pending"].take()).filter(|p| !p.is_null());
        Ok(Saved {
            history,
            log,
            context,
            choices,
            pending,
        })
    }

    pub fn delete(&self, id: &str) -> io::Result<()> {
        if !valid_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bad session id",
            ));
        }
        fs::remove_file(self.path(id))
    }

    /// Every stored session file with its id.
    fn files(&self) -> Vec<(String, Value)> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path
                    .file_name()?
                    .to_str()?
                    .strip_suffix(".json")?
                    .to_owned();
                if !valid_id(&id) {
                    return None;
                }
                let file: Value = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
                Some((id, file))
            })
            .collect()
    }

    /// `{id, title, updated, waiting}` of every stored session, newest first. `waiting`:
    /// a webhook run paused for the user's approval.
    pub fn list(&self) -> Vec<Value> {
        let mut sessions: Vec<Value> = self
            .files()
            .into_iter()
            .map(|(id, file)| {
                json!({
                    "id": id,
                    "title": file["title"],
                    "updated": file["updated"],
                    "waiting": !file["pending"].is_null(),
                })
            })
            .collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s["updated"].as_u64().unwrap_or(0)));
        sessions
    }

    /// `{id, updated, pending}` of the sessions waiting for approval, oldest first.
    pub fn waiting(&self) -> Vec<Value> {
        let mut sessions: Vec<Value> = self
            .files()
            .into_iter()
            .filter(|(_, file)| !file["pending"].is_null())
            .map(|(id, file)| json!({"id": id, "updated": file["updated"], "pending": file["pending"]}))
            .collect();
        sessions.sort_by_key(|s| s["updated"].as_u64().unwrap_or(0));
        sessions
    }

    #[cfg(test)]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("agent_sessions_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn saves_lists_loads_and_deletes() {
        let store = temp("roundtrip");
        assert!(store.list().is_empty());
        let id = store.new_id();
        let log = vec![json!({"type": "user", "text": "hello\nthere"})];
        let history = vec![Message::user("hello there")];
        store
            .save(
                &id,
                &history,
                &log,
                Some(&json!({"tokens": 3})),
                &Choices {
                    tools: Some(vec!["execute".to_owned()]),
                    skills_disabled: Some(vec!["paperless".to_owned()]),
                },
                None,
            )
            .unwrap();
        assert_ne!(store.new_id(), id);

        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(
            (&list[0]["id"], &list[0]["title"]),
            (&json!(id), &json!("hello there"))
        );

        let saved = store.load(&id).unwrap();
        assert_eq!(saved.history, history);
        assert_eq!(saved.log, log);
        assert_eq!(saved.context, Some(json!({"tokens": 3})));
        assert_eq!(saved.choices.tools, Some(vec!["execute".to_owned()]));
        assert_eq!(saved.choices.skills_disabled, Some(vec!["paperless".to_owned()]));
        assert_eq!(saved.pending, None);
        assert_eq!(list[0]["waiting"], false);
        assert!(store.waiting().is_empty());

        store.delete(&id).unwrap();
        assert!(store.list().is_empty());
        let _ = fs::remove_dir_all(store.dir());
    }

    #[test]
    fn a_pending_call_marks_the_session_as_waiting() {
        let store = temp("pending");
        let pending = json!({"hook": "h", "call": {"id": "c1"}});
        store.save("a", &[], &[], None, &Choices::default(), Some(&pending)).unwrap();
        store.save("b", &[], &[], None, &Choices::default(), None).unwrap();
        assert_eq!(store.load("a").unwrap().pending, Some(pending.clone()));
        let waiting = store.waiting();
        assert_eq!((waiting.len(), &waiting[0]["id"], &waiting[0]["pending"]), (1, &json!("a"), &pending));
        let waits = |id: &str| store.list().iter().find(|s| s["id"] == id).unwrap()["waiting"].clone();
        assert_eq!((waits("a"), waits("b")), (json!(true), json!(false)));
        // Saving without it ends the waiting.
        store.save("a", &[], &[], None, &Choices::default(), None).unwrap();
        assert!(store.waiting().is_empty());
        let _ = fs::remove_dir_all(store.dir());
    }

    #[test]
    fn ids_cannot_escape_the_directory() {
        let store = temp("ids");
        for bad in ["", "../x", "a/b", "a.b", "x\0"] {
            assert!(!valid_id(bad), "{bad:?}");
            assert!(store.load(bad).is_err());
            assert!(store.save(bad, &[], &[], None, &Choices::default(), None).is_err());
        }
    }
}
