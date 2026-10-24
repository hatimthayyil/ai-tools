use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::{
    agent,
    file::{AtPath, pretty},
    memory, project, summariser,
};

/// The settings file: the user's in [`home`], a project's at or above where `ai` runs.
pub const FILE: &str = "ai.toml";

/// One user's overrides of a project's settings, beside its [`FILE`].
pub const LOCAL: &str = "ai.local.toml";

/// `~/.ai`: the user's settings, and every store not pointed elsewhere.
pub fn home() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(".ai"))
}

fn user_file() -> Option<PathBuf> {
    home().map(|h| h.join(FILE))
}

/// The project's `ai.toml`: the nearest at or above `cwd`, never the user's.
pub fn project_file(cwd: &Path) -> Option<PathBuf> {
    nearest(user_file().as_deref(), cwd)
}

/// Whether `p` is the user's `ai.toml`.
pub fn is_user(p: &Path) -> bool {
    user_file().is_some_and(|u| same(&u, p))
}

fn nearest(user: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|d| d.join(FILE))
        .filter(|p| !user.is_some_and(|u| same(u, p)))
        .find(|p| p.is_file())
}

fn same(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// One `ai.toml` as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct File {
    pub project: Option<project::Table>,
    pub chat: agent::Config,
    pub agent: agent::Table,
    pub memory: memory::Config,
    pub summariser: summariser::Config,
    pub agents: BTreeMap<String, Agent>,
}

/// `[agents.<name>]`: what an agent runs with. A relative `instructions` is from the folder of the
/// file that sets it, a relative `worktree` from the project's [`home`](project::home), so every
/// checkout of the project names the same directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Agent {
    /// The Markdown files of the agent's instructions, one from each file that names one: the
    /// user's, then the project's, then the local one.
    #[serde(deserialize_with = "paths")]
    pub instructions: Vec<PathBuf>,
    /// The directory the agent runs in.
    #[serde(deserialize_with = "path")]
    pub worktree: Option<PathBuf>,
}

fn path<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
    match String::deserialize(d)?.trim() {
        "" => Err(D::Error::custom(
            "is empty: name a path, or leave the key out",
        )),
        p => Ok(Some(p.into())),
    }
}

fn paths<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PathBuf>, D::Error> {
    Ok(path(d)?.into_iter().collect())
}

impl Agent {
    fn over(self, base: Agent) -> Agent {
        Agent {
            instructions: [base.instructions, self.instructions].concat(),
            worktree: self.worktree.or(base.worktree),
        }
    }

    fn from(self, dir: &Path) -> Agent {
        Agent {
            instructions: self.instructions.iter().map(|p| dir.join(p)).collect(),
            ..self
        }
    }

    fn under(self, root: &Path) -> Agent {
        Agent {
            worktree: self.worktree.map(|p| root.join(p)),
            ..self
        }
    }
}

/// The `ai.toml` at `p`, `None` without the file; a bad key or value names both.
pub(crate) fn read(p: &Path) -> Result<Option<File>> {
    let text = match fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).at(p),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|e| anyhow!("{}: {e}", pretty(p)))
}

/// What `ai.toml` sets: the project's `ai.local.toml` over its `ai.toml` over the user's, key by
/// key, except an agent's instructions, which add up; unset keys keep their defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub chat: agent::Config,
    pub agent: agent::Table,
    pub memory: memory::Config,
    pub summariser: summariser::Config,
    agents: BTreeMap<String, Agent>,
}

impl Settings {
    /// The user's `ai.toml`, under the nearest one at or above `cwd`, under the `ai.local.toml`
    /// beside that in the project's [`home`](project::home).
    pub fn find(cwd: &Path) -> Result<Settings> {
        Settings::load(user_file().as_deref(), cwd)
    }

    /// [`Settings::find`] with the user's file at `user`.
    pub fn load(user: Option<&Path>, cwd: &Path) -> Result<Settings> {
        let project = nearest(user, cwd);
        let home = match project.as_deref().and_then(Path::parent) {
            Some(dir) => Some(project::home(dir)?),
            None => None,
        };
        let local = home.as_ref().map(|h| h.join(LOCAL));
        [
            user.map(|p| (p, Layer::User)),
            project.as_deref().map(|p| (p, Layer::Project)),
            local.as_deref().map(|p| (p, Layer::Local)),
        ]
        .into_iter()
        .flatten()
        .try_fold(Settings::default(), |base, (p, layer)| {
            Ok(Settings::read(p, layer)?.unwrap_or_default().over(base))
        })
        .map(|s| match &home {
            Some(home) => s.under(home),
            None => s,
        })
    }

    /// `[agents.<name>]`, its paths absolute, except a relative `worktree` outside a project;
    /// `None` when no file has the table.
    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.get(name)
    }

    /// The settings in `p`, `None` without the file; only a project's `ai.toml` holds
    /// `[project]`.
    fn read(p: &Path, layer: Layer) -> Result<Option<Settings>> {
        let Some(f) = read(p)? else {
            return Ok(None);
        };
        if f.project.is_some() {
            match layer {
                Layer::User => bail!(
                    "{}: [project] belongs in a project's {FILE}, not in your own.",
                    pretty(p)
                ),
                Layer::Local => bail!(
                    "{}: [project] belongs in the project's {FILE}, not in {LOCAL}.",
                    pretty(p)
                ),
                Layer::Project => {}
            }
        }
        Ok(Some(Settings {
            chat: f.chat,
            agent: f.agent,
            memory: f.memory,
            summariser: f.summariser,
            agents: f
                .agents
                .into_iter()
                .map(|(name, a)| (name, a.from(p.parent().unwrap_or(Path::new("")))))
                .collect(),
        }))
    }

    fn over(self, base: Settings) -> Settings {
        let mut agents = base.agents;
        for (name, a) in self.agents {
            let b = agents.remove(&name).unwrap_or_default();
            agents.insert(name, a.over(b));
        }
        Settings {
            chat: self.chat.over(base.chat),
            agent: self.agent.over(base.agent),
            memory: self.memory.over(base.memory),
            summariser: self.summariser.over(base.summariser),
            agents,
        }
    }

    fn under(self, root: &Path) -> Settings {
        Settings {
            agents: self
                .agents
                .into_iter()
                .map(|(name, a)| (name, a.under(root)))
                .collect(),
            ..self
        }
    }
}

/// Where a settings file sits, which decides whether it may hold `[project]`.
#[derive(Clone, Copy)]
enum Layer {
    User,
    Project,
    Local,
}
