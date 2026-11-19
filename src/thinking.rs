//! Thinking levels of the model, as Ollama reports them.
//!
//! `POST /api/show` lists what a model accepts for `think` in `thinking.values`
//! (`[false, true]` for on/off models, `[false, "low", "medium", "xhigh"]` for Qwen3.8,
//! absent for models that don't think). Only what is listed there is offered; the other
//! backends tell nothing like it, so they offer nothing.

use rig_core::serde_json::{Value, json};
use std::time::Duration;

/// The levels a model accepts, in the order Ollama lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct Thinking {
    /// `off`, `on` or the model's own names (`low`, `medium`, `xhigh`, ...).
    pub levels: Vec<String>,
    /// What the model uses when no `think` is sent.
    pub default: String,
}

fn name(value: &Value) -> Option<String> {
    match value {
        Value::Bool(false) => Some("off".into()),
        Value::Bool(true) => Some("on".into()),
        Value::String(level) => Some(level.clone()),
        _ => None,
    }
}

/// The value to send as `think` for `level`.
pub fn param(level: &str) -> Value {
    match level {
        "off" => json!(false),
        "on" => json!(true),
        other => json!(other),
    }
}

impl Thinking {
    /// Reads the `thinking` field of an `/api/show` answer; `None` if the model has
    /// nothing to choose.
    pub fn parse(show: &Value) -> Option<Self> {
        let thinking = show.get("thinking")?;
        let levels: Vec<String> = thinking
            .get("values")?
            .as_array()?
            .iter()
            .filter_map(name)
            .collect();
        if levels.len() < 2 {
            return None;
        }
        let default = thinking
            .get("default")
            .and_then(name)
            .filter(|d| levels.contains(d))
            .unwrap_or_else(|| levels[0].clone());
        Some(Self { levels, default })
    }

    /// Asks the daemon at `base_url` about `model`; `None` if it can't say.
    pub async fn discover(base_url: &str, api_key: Option<&str>, model: &str) -> Option<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        let mut request = client
            .post(format!("{}/api/show", base_url.trim_end_matches('/')))
            .json(&json!({"model": model}));
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }
        let show: Value = request
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        Self::parse(&show)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_levels_come_in_the_models_order() {
        let show =
            json!({"thinking": {"values": [false, "low", "medium", "xhigh"], "default": "medium"}});
        let thinking = Thinking::parse(&show).unwrap();
        assert_eq!(thinking.levels, ["off", "low", "medium", "xhigh"]);
        assert_eq!(thinking.default, "medium");
    }

    #[test]
    fn on_off_models_get_a_switch() {
        let show = json!({"thinking": {"values": [false, true], "default": true}});
        let thinking = Thinking::parse(&show).unwrap();
        assert_eq!(thinking.levels, ["off", "on"]);
        assert_eq!(thinking.default, "on");
    }

    #[test]
    fn nothing_to_choose_offers_nothing() {
        assert_eq!(Thinking::parse(&json!({"thinking": null})), None);
        assert_eq!(
            Thinking::parse(&json!({"capabilities": ["completion"]})),
            None
        );
        assert_eq!(
            Thinking::parse(&json!({"thinking": {"values": [false], "default": false}})),
            None
        );
    }

    #[test]
    fn levels_are_sent_as_ollama_names_them() {
        assert_eq!(param("off"), json!(false));
        assert_eq!(param("on"), json!(true));
        assert_eq!(param("xhigh"), json!("xhigh"));
    }
}
