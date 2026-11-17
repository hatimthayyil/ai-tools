use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use genai::chat::ReasoningEffort;
use serde::{Deserialize, Deserializer, de::Error};

use crate::{
    config::positive,
    error,
    memory::{Api, Backend, ClaudeCode, JOBS, MakeBackend, Provider, Witnessed, claude},
};

/// How long `build --rebuild` waits for a running summariser.
pub const WAIT: Duration = Duration::from_secs(60);

/// What an error of the witness's own settings is told under.
const WITNESS: &str = "Cannot start the witness of `[summariser.witness]`";

/// The settings `read` gave, if they can make a backend ([`Config::check`]). If they can't, says
/// why on stderr and gives none: the summariser a command starts in the background tells its
/// errors only to its log, so the command that would start it is where a person sees them.
pub fn usable(read: Result<Config>) -> Option<Config> {
    match read.and_then(|c| c.check().map(|()| c)) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!(
                "No summariser started, so summaries stay out of date until this is fixed: {}",
                error::line(&e)
            );
            None
        }
    }
}

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
    /// `[summariser.keys]`: for [`Kind::Api`], the shell command that prints each provider's
    /// key, by the provider's name in `model`. A key goes only to the provider it is set for; a
    /// provider with none takes its key from its environment variable. Only the user's own
    /// `ai.toml` may set it, as [`Settings::summariser`](crate::config::Settings::summariser)
    /// checks.
    #[serde(deserialize_with = "keys")]
    pub keys: BTreeMap<String, String>,
    /// `[summariser.witness]`: the model that checks every summary ([`Witnessed`]), set by
    /// `backend`, `model` and `effort`, whose defaults are those above, its key the one
    /// `[summariser.keys]` sets for its provider; unset, no witness.
    pub witness: Option<Box<Config>>,
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

fn keys<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    let mut keys = BTreeMap::<String, String>::deserialize(d)?;
    for (provider, command) in &mut keys {
        Provider::named(provider, "[summariser.keys]").map_err(D::Error::custom)?;
        *command = command.trim().to_string();
        if command.is_empty() {
            return Err(D::Error::custom(format!(
                "`{provider}` is empty: give a command that prints the key, or leave it out"
            )));
        }
    }
    Ok(keys)
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
            keys: base.keys.into_iter().chain(self.keys).collect(),
            witness: match (self.witness, base.witness) {
                (Some(w), Some(b)) => Some(Box::new(w.over(*b))),
                (w, b) => w.or(b),
            },
        }
    }

    /// The first table of these settings that names a key's command, as a file names it:
    /// `[summariser.keys]`, else `[summariser.witness.keys]`.
    pub fn keys_table(&self) -> Option<&'static str> {
        match (self.keys.is_empty(), self.witness.as_deref()) {
            (false, _) => Some("[summariser.keys]"),
            (true, Some(w)) if !w.keys.is_empty() => Some("[summariser.witness.keys]"),
            _ => None,
        }
    }

    /// `[summariser.witness]`, if set; an error if it sets `jobs`, keys or a witness of its own.
    pub fn witness(&self) -> Result<Option<&Config>> {
        let Some(w) = self.witness.as_deref() else {
            return Ok(None);
        };
        if w.jobs.is_some() || w.witness.is_some() || !w.keys.is_empty() {
            bail!(
                "`[summariser.witness]` takes `backend`, `model` and `effort` alone: the witness \
                 checks within each summariser call, so it has no `jobs`, and no witness of its \
                 own, and its key is the one `[summariser.keys]` sets for its provider."
            );
        }
        Ok(Some(w))
    }

    /// What stops these settings making their backend, as far as shows without running a
    /// command or a model: a witness table with a key it can't take, an effort or a model the
    /// backend can't take, a provider with no key set.
    pub fn check(&self) -> Result<()> {
        self.checked(&self.keys)?;
        match self.witness()? {
            Some(w) => w.checked(&self.keys).context(WITNESS),
            None => Ok(()),
        }
    }

    /// [`Config::check`] for the model these settings name, alone, its key among `keys`.
    fn checked(&self, keys: &BTreeMap<String, String>) -> Result<()> {
        match self.kind() {
            Kind::Claude => self
                .effort
                .as_ref()
                .map_or(Ok(()), |e| e.claude().map(drop)),
            Kind::Api => Provider::of(&self.model())?.key_command(keys).map(drop),
        }
    }

    /// The backend, with `model` over the configured one, keeping its calls in `calls`; with
    /// `[summariser.witness]`, the [`Witnessed`] pair, the witness keeping its calls there too.
    pub fn backend(&self, model: Option<&str>, calls: &Path) -> Result<Arc<dyn Backend>> {
        let writer = self.writer(model, &self.keys, calls)?;
        Ok(match self.witness()? {
            None => writer,
            Some(w) => {
                let witness = w.writer(None, &self.keys, calls).context(WITNESS)?;
                Arc::new(Witnessed::new(writer, witness))
            }
        })
    }

    /// The model these settings name, alone, its key among `keys`.
    fn writer(
        &self,
        model: Option<&str>,
        keys: &BTreeMap<String, String>,
        calls: &Path,
    ) -> Result<Arc<dyn Backend>> {
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
                Arc::new(Api::new(&model, effort, jobs, keys, calls)?)
            }
        })
    }

    /// [`Config::backend`] with the configured model, for a store's runtime.
    pub fn maker(self) -> Box<MakeBackend> {
        Box::new(move |calls| self.backend(None, calls))
    }
}
