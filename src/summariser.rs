use std::{path::Path, sync::Arc};

use anyhow::Result;
use serde::{Deserialize, Deserializer, de::Error};

use crate::memory::{Backend, ClaudeCode, MakeBackend, claude};

/// `[summariser]` in `ai.toml`: what writes every summary, for agents, memory and notes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Default [`Kind::Claude`].
    pub backend: Option<Kind>,
    /// In the backend's own spelling; default Sonnet.
    #[serde(deserialize_with = "model")]
    pub model: Option<String>,
}

/// Where a summary is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Claude Code in print mode, `claude -p`.
    Claude,
}

fn model<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match String::deserialize(d)?.trim() {
        "" => Err(D::Error::custom(
            "is empty: name a model, or leave the key out",
        )),
        m => Ok(Some(m.into())),
    }
}

impl Config {
    pub fn kind(&self) -> Kind {
        self.backend.unwrap_or(Kind::Claude)
    }

    pub fn model(&self) -> String {
        self.model.clone().unwrap_or_else(|| "sonnet".into())
    }

    /// These settings, then `base`'s for those unset here.
    pub fn over(self, base: Config) -> Config {
        Config {
            backend: self.backend.or(base.backend),
            model: self.model.or(base.model),
        }
    }

    /// The backend, with `model` over the configured one, keeping its calls in `calls`.
    pub fn backend(&self, model: Option<&str>, calls: &Path) -> Result<Arc<dyn Backend>> {
        let model = model.map_or_else(|| self.model(), str::to_string);
        Ok(match self.kind() {
            Kind::Claude => Arc::new(ClaudeCode::compactor_at(claude::find()?, &model, calls)?),
        })
    }

    /// [`Config::backend`] with the configured model, for a store's runtime.
    pub fn maker(self) -> Box<MakeBackend> {
        Box::new(move |calls| self.backend(None, calls))
    }
}
