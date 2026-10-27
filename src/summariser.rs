use std::{path::Path, sync::Arc};

use anyhow::{Result, bail};
use genai::chat::ReasoningEffort;
use serde::{Deserialize, Deserializer, de::Error};

use crate::{
    config::positive,
    memory::{Api, Backend, ClaudeCode, JOBS, MakeBackend, claude},
};

/// `[summariser]` in `ai.toml`: what writes every summary, for agents, memory and notes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Default [`Kind::Claude`].
    pub backend: Option<Kind>,
    /// In the backend's own spelling, `<provider>/<model>` for [`Kind::Api`]; default Sonnet.
    #[serde(deserialize_with = "model")]
    pub model: Option<String>,
    /// How hard the model thinks; unset, the backend's own default: medium for
    /// [`Kind::Claude`], the provider's for [`Kind::Api`].
    pub effort: Option<Effort>,
    /// Calls a build runs at once; default [`JOBS`].
    #[serde(deserialize_with = "positive")]
    pub jobs: Option<usize>,
}

/// Where a summary is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Claude Code in print mode, `claude -p`.
    Claude,
    /// A provider's API through `genai`; see [`Api`].
    Api,
}

/// `[summariser] effort`: one of genai's reasoning efforts, `zero` (or `none`), `minimal`,
/// `low`, `medium`, `high`, `xhigh` and `max`, or a budget of thinking tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effort(String);

/// The efforts `claude --effort` takes.
const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

impl Effort {
    pub fn reasoning(&self) -> ReasoningEffort {
        self.0.parse().expect("an effort is checked as it is read")
    }

    /// This effort as `claude --effort` takes it; an error if it takes no such level.
    pub fn claude(&self) -> Result<&str> {
        match CLAUDE_EFFORTS.contains(&self.0.as_str()) {
            true => Ok(&self.0),
            false => bail!(
                "`[summariser] effort = \"{}\"` is no effort `claude -p` takes: with \
                 `backend = \"claude\"`, use {}.",
                self.0,
                CLAUDE_EFFORTS.join(", ")
            ),
        }
    }
}

impl<'de> Deserialize<'de> for Effort {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Effort, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            Budget(i64),
        }
        let given = match Raw::deserialize(d)? {
            Raw::Word(w) => w.trim().to_string(),
            Raw::Budget(n) if n > 0 && u32::try_from(n).is_ok() => n.to_string(),
            Raw::Budget(n) => {
                return Err(D::Error::custom(format!(
                    "{n} is no budget: give at least 1 thinking token"
                )));
            }
        };
        match given.parse::<ReasoningEffort>() {
            Ok(ReasoningEffort::Budget(0)) | Err(_) => Err(D::Error::custom(format!(
                "`{given}` is no effort: use zero, minimal, low, medium, high, xhigh, max, or a \
                 budget of thinking tokens"
            ))),
            Ok(e) => Ok(Effort(e.to_string())),
        }
    }
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

    pub fn jobs(&self) -> usize {
        self.jobs.unwrap_or(JOBS)
    }

    /// These settings, then `base`'s for those unset here.
    pub fn over(self, base: Config) -> Config {
        Config {
            backend: self.backend.or(base.backend),
            model: self.model.or(base.model),
            effort: self.effort.or(base.effort),
            jobs: self.jobs.or(base.jobs),
        }
    }

    /// The backend, with `model` over the configured one, keeping its calls in `calls`.
    pub fn backend(&self, model: Option<&str>, calls: &Path) -> Result<Arc<dyn Backend>> {
        let model = model.map_or_else(|| self.model(), str::to_string);
        let jobs = self.jobs();
        Ok(match self.kind() {
            Kind::Claude => {
                let effort = self.effort.as_ref().map(Effort::claude).transpose()?;
                let c = ClaudeCode::compactor_at(claude::find()?, &model, calls)?.with_jobs(jobs);
                Arc::new(match effort {
                    Some(e) => c.with_effort(e),
                    None => c,
                })
            }
            Kind::Api => {
                let effort = self.effort.as_ref().map(Effort::reasoning);
                Arc::new(Api::new(&model, effort, jobs, calls)?)
            }
        })
    }

    /// [`Config::backend`] with the configured model, for a store's runtime.
    pub fn maker(self) -> Box<MakeBackend> {
        Box::new(move |calls| self.backend(None, calls))
    }
}
