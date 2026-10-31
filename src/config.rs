use std::{
    collections::{BTreeMap, BTreeSet},
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

/// The link in a project's top directory to its [private settings](private_file).
pub const LOCAL: &str = "ai.local.toml";

/// One user's overrides of the settings of the project at `home`: an `ai.toml` in its private
/// store.
pub fn private_file(home: &Path) -> PathBuf {
    home.join(project::STORE).join(FILE)
}

/// Refuses an [`LOCAL`] in `home` that is a file of its own rather than the link to
/// [`private_file`].
pub fn check_link(home: &Path) -> Result<()> {
    let p = home.join(LOCAL);
    match fs::symlink_metadata(&p) {
        Ok(m) if !m.file_type().is_symlink() => bail!(
            "{} is a file of its own. Move it to {}, then run `ai init` to link it.",
            pretty(&p),
            pretty(&private_file(home))
        ),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).at(&p),
    }
}

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
/// file that sets it, or for the private settings from the project's [`home`](project::home), where
/// their [link](LOCAL) sits; a relative `worktree` is from the home, so every checkout of the
/// project names the same directory.
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
    /// The agents it reports to, each named by its own table; `None` when no file sets the key.
    #[serde(rename = "reports-to", deserialize_with = "superiors")]
    pub reports_to: Option<Vec<String>>,
}

fn path<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
    match String::deserialize(d)?.trim() {
        "" => Err(D::Error::custom(
            "is empty: name a path, or leave the key out",
        )),
        p => Ok(Some(p.into())),
    }
}

/// A whole number of 1 or more.
pub(crate) fn positive<'de, D: Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    match usize::deserialize(d)? {
        0 => Err(D::Error::custom("must be a positive whole number")),
        n => Ok(Some(n)),
    }
}

fn paths<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PathBuf>, D::Error> {
    Ok(path(d)?.into_iter().collect())
}

/// Agent names, each a [thread name](agent::check_name), none twice.
fn superiors<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    let names = Vec::<String>::deserialize(d)?;
    let mut seen = BTreeSet::new();
    for n in &names {
        agent::check_name(n).map_err(D::Error::custom)?;
        if !seen.insert(n) {
            return Err(D::Error::custom(format!("names {n} twice")));
        }
    }
    Ok(Some(names))
}

impl Agent {
    fn over(self, base: Agent) -> Agent {
        Agent {
            instructions: [base.instructions, self.instructions].concat(),
            worktree: self.worktree.or(base.worktree),
            reports_to: self.reports_to.or(base.reports_to),
        }
    }

    /// The agents it reports to; none when no file sets `reports-to`.
    pub fn superiors(&self) -> &[String] {
        self.reports_to.as_deref().unwrap_or_default()
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

/// What `ai.toml` sets: the project's private settings over its `ai.toml` over the user's, key by
/// key, except an agent's instructions, which add up; unset keys keep their defaults. The agents'
/// `reports-to` form a hierarchy with no cycle, each naming agents that have a table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub chat: agent::Config,
    pub agent: agent::Table,
    pub memory: memory::Config,
    pub summariser: summariser::Config,
    agents: BTreeMap<String, Agent>,
}

impl Settings {
    /// The user's `ai.toml`, under the nearest one at or above `cwd`, under the
    /// [private settings](private_file) of that project's [`home`](project::home).
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
        if let Some(h) = &home {
            check_link(h)?;
        }
        let local = home.as_deref().map(|h| (private_file(h), h));
        let beside = |p: &Path| p.parent().unwrap_or(Path::new("")).to_path_buf();
        [
            user.map(|p| (p.to_path_buf(), beside(p), Layer::User)),
            project
                .as_deref()
                .map(|p| (p.to_path_buf(), beside(p), Layer::Project)),
            local.map(|(p, h)| (p, h.to_path_buf(), Layer::Local)),
        ]
        .into_iter()
        .flatten()
        .try_fold(Settings::default(), |base, (p, dir, layer)| {
            Ok(Settings::read(&p, &dir, layer)?
                .unwrap_or_default()
                .over(base))
        })
        .map(|s| match &home {
            Some(home) => s.under(home),
            None => s,
        })
        .and_then(|s| {
            s.check_hierarchy()?;
            Ok(s)
        })
    }

    /// `[agents.<name>]`, its paths absolute, except a relative `worktree` outside a project;
    /// `None` when no file has the table.
    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.get(name)
    }

    /// The agents below agent `name`: those that report to it, and every agent below them, through
    /// any number of levels. These are the threads `name` may read besides its own; its superiors
    /// and siblings are never among them.
    pub fn below(&self, name: &str) -> BTreeSet<&str> {
        let mut found = BTreeSet::new();
        let mut next = vec![name];
        while let Some(boss) = next.pop() {
            for (n, a) in &self.agents {
                if a.superiors().iter().any(|s| s == boss) && found.insert(n.as_str()) {
                    next.push(n);
                }
            }
        }
        found
    }

    /// Refuses an agent that reports to itself, to an agent no table defines, or, through others,
    /// to an agent below it.
    fn check_hierarchy(&self) -> Result<()> {
        for (n, a) in &self.agents {
            for s in a.superiors() {
                if s == n {
                    bail!("Agent {n} reports to itself. Take {n} out of its reports-to.");
                }
                if !self.agents.contains_key(s) {
                    bail!(
                        "Agent {n} reports to {s}, which no [agents.{s}] table defines. Add the \
                         table to an {FILE}, or take {s} out of {n}'s reports-to."
                    );
                }
            }
        }
        let mut done = BTreeSet::new();
        for n in self.agents.keys() {
            if let Some(cycle) = self.cycle_from(n, &mut Vec::new(), &mut done) {
                bail!(
                    "Agents report to each other in a cycle: {}. Take one name out of a \
                     reports-to.",
                    cycle.join(" -> ")
                );
            }
        }
        Ok(())
    }

    /// The first cycle of superiors reachable from `n`, its first agent repeated at its end, by a
    /// depth-first walk; `path` holds the agents walked to reach `n`, `done` those no cycle passes.
    fn cycle_from<'a>(
        &'a self,
        n: &'a str,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<&'a str>> {
        if done.contains(n) {
            return None;
        }
        if let Some(at) = path.iter().position(|p| *p == n) {
            let mut cycle = path[at..].to_vec();
            cycle.push(n);
            return Some(cycle);
        }
        path.push(n);
        for s in self.agents.get(n).map_or(&[][..], Agent::superiors) {
            if let Some(cycle) = self.cycle_from(s, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        done.insert(n);
        None
    }

    /// The settings in `p`, its relative instructions from `dir`, `None` without the file; only
    /// a project's `ai.toml` holds `[project]`.
    fn read(p: &Path, dir: &Path, layer: Layer) -> Result<Option<Settings>> {
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
                    "{}: [project] belongs in the project's {FILE}, not in its private one.",
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
                .map(|(name, a)| (name, a.from(dir)))
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
