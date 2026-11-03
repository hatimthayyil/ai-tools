use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io,
    marker::PhantomData,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeOwned, DeserializeSeed, Error as _, IgnoredAny, MapAccess, Visitor},
};

use crate::{
    agent,
    file::{AtPath, pretty},
    memory,
    project::{self, Project},
    summariser,
};

/// The settings file: the user's in [`home`], a project's at or above where `ai` runs.
pub const FILE: &str = "ai.toml";

/// The link in a project's top directory to its [private settings](Project::private).
pub const LOCAL: &str = "ai.local.toml";

/// Refuses an [`LOCAL`] in `home` that is a file of its own rather than the link to `private`,
/// the project's private settings.
pub fn check_link(home: &Path, private: &Path) -> Result<()> {
    let p = home.join(LOCAL);
    match fs::symlink_metadata(&p) {
        Ok(m) if !m.file_type().is_symlink() => bail!(
            "{} is a file of its own. Move it to {}, then run `ai init` to link it.",
            pretty(&p),
            pretty(private)
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

/// Where `ai` keeps one user's stores. `repo` is the user's git repository, `~/ai`: every
/// thread and each project's private settings, what a new machine needs. `machine` is `~/.ai`:
/// what belongs to this machine and never goes in git, such as locks, logs and archives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Homes {
    pub repo: PathBuf,
    pub machine: PathBuf,
}

impl Homes {
    /// `~/ai` and `~/.ai`.
    pub fn user() -> Result<Homes> {
        let home = std::env::home_dir().context("Cannot find your home directory: set HOME.")?;
        Ok(Homes::at(&home))
    }

    /// `ai` and `.ai` in `dir`, as a home directory holds them.
    pub fn at(dir: &Path) -> Homes {
        Homes {
            repo: dir.join("ai"),
            machine: dir.join(".ai"),
        }
    }
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

/// The tables an `ai.toml` may hold, each read on its own by the commands that use it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tables {
    project: Option<IgnoredAny>,
    #[serde(rename = "chat")]
    _chat: Option<IgnoredAny>,
    #[serde(rename = "agent")]
    _agent: Option<IgnoredAny>,
    #[serde(rename = "memory")]
    _memory: Option<IgnoredAny>,
    #[serde(rename = "summariser")]
    _summariser: Option<IgnoredAny>,
    #[serde(rename = "agents")]
    _agents: Option<IgnoredAny>,
}

/// `[agents.<name>]`: what an agent runs with. A relative `instructions` is from the folder of the
/// file that sets it, or for the private settings from the project's [`home`](project::home), where
/// their [link](LOCAL) sits; a relative `worktree` is from the home, so every checkout of the
/// project names the same directory. Its superiors are the [`Hierarchy`]'s.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agent {
    /// The Markdown files of the agent's instructions, one from each file that names one: the
    /// user's, then the project's, then the local one.
    pub instructions: Vec<PathBuf>,
    /// The directory the agent runs in.
    pub worktree: Option<PathBuf>,
}

/// `[agents.<name>]` read for the agent itself: `reports-to` is only named, so a bad hierarchy
/// stops nothing that runs the agent. Its keys are [`Rank`]'s.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Keys {
    #[serde(deserialize_with = "paths")]
    instructions: Vec<PathBuf>,
    #[serde(deserialize_with = "path")]
    worktree: Option<PathBuf>,
    #[serde(rename = "reports-to")]
    _reports_to: IgnoredAny,
}

/// `[agents.<name>]` read for the [`Hierarchy`]: the other keys are only named, so a bad one
/// stops nothing that reads the hierarchy. Its keys are [`Keys`]'.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Rank {
    /// `None` when the file does not set the key.
    #[serde(rename = "reports-to", deserialize_with = "superiors")]
    reports_to: Option<Vec<String>>,
    #[serde(rename = "instructions")]
    _instructions: IgnoredAny,
    #[serde(rename = "worktree")]
    _worktree: IgnoredAny,
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
        }
    }

    fn under(self, root: &Path) -> Agent {
        Agent {
            worktree: self.worktree.map(|p| root.join(p)),
            ..self
        }
    }
}

/// What `seed` reads from key `key` of a table, the table's other keys skipped unread; `None`
/// without the key.
struct Only<'a, S> {
    key: &'a str,
    seed: S,
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Only<'_, S> {
    type Value = Option<S::Value>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de, S: DeserializeSeed<'de>> Visitor<'de> for Only<'_, S> {
    type Value = Option<S::Value>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a table")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seed = Some(self.seed);
        let mut found = None;
        while let Some(k) = map.next_key::<String>()? {
            match seed.take_if(|_| k == self.key) {
                Some(s) => found = Some(map.next_value_seed(s)?),
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(found)
    }
}

/// One `ai.toml` that exists: where it is, the folder its relative instructions are from, and
/// its text, which is TOML holding only the tables its place allows.
struct Source {
    path: PathBuf,
    dir: PathBuf,
    text: String,
}

impl Source {
    /// The `ai.toml` at `path`, `None` without the file. Refuses text that is not TOML, a table
    /// no part of the settings has, and `[project]` outside a project's own file: each leaves
    /// every part of the file in doubt.
    fn read(path: &Path, dir: PathBuf, layer: Layer) -> Result<Option<Source>> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).at(path),
        };
        let s = Source {
            path: path.to_path_buf(),
            dir,
            text,
        };
        let tables: Tables = s.de(PhantomData)?;
        if tables.project.is_some() {
            match layer {
                Layer::User => bail!(
                    "{}: [project] belongs in a project's {FILE}, not in your own.",
                    pretty(path)
                ),
                Layer::Local => bail!(
                    "{}: [project] belongs in the project's {FILE}, not in its private one.",
                    pretty(path)
                ),
                Layer::Project => {}
            }
        }
        Ok(Some(s))
    }

    /// What `seed` reads from the file; a bad key or value names the file.
    fn de<'a, S: DeserializeSeed<'a>>(&'a self, seed: S) -> Result<S::Value> {
        toml::Deserializer::parse(&self.text)
            .and_then(|d| seed.deserialize(d))
            .map_err(|e| anyhow!("{}: {e}", pretty(&self.path)))
    }

    /// Table `key`, `None` when the file has none.
    fn part<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.de(Only {
            key,
            seed: PhantomData,
        })
    }
}

/// `[project]` of the project's `ai.toml` at `p`, `None` without the file or the table. Only the
/// file as a whole and that table are checked: a bad key elsewhere in it stops nothing that needs
/// the project.
pub(crate) fn project_table(p: &Path) -> Result<Option<project::Table>> {
    let dir = p.parent().unwrap_or(Path::new("")).to_path_buf();
    match Source::read(p, dir, Layer::Project)? {
        Some(s) => s.part("project"),
        None => Ok(None),
    }
}

/// What `ai.toml` sets: the project's private settings over its `ai.toml` over the user's, key by
/// key, except an agent's instructions, which add up; unset keys keep their defaults.
///
/// Each part is read when a command asks for it, from the files as they are then, so a bad key
/// or value stops only the commands that read its part. A file that is not TOML, or holds a table
/// no part has, stops every part read from it: what it was meant to set is unknown.
#[derive(Clone, Debug)]
pub struct Settings {
    user: Option<PathBuf>,
    homes: Option<Homes>,
    cwd: PathBuf,
}

impl Settings {
    /// The user's `ai.toml`, under the nearest one at or above `cwd`, under that project's
    /// [private settings](Project::private) in `~/ai`.
    pub fn find(cwd: &Path) -> Settings {
        Settings::load(user_file().as_deref(), Homes::user().ok().as_ref(), cwd)
    }

    /// [`Settings::find`] with the private settings under `homes`.
    pub fn find_in(homes: &Homes, cwd: &Path) -> Settings {
        Settings::load(user_file().as_deref(), Some(homes), cwd)
    }

    /// [`Settings::find`] with the user's file at `user` and the private settings under
    /// `homes`; without `homes`, no private settings.
    pub fn load(user: Option<&Path>, homes: Option<&Homes>, cwd: &Path) -> Settings {
        Settings {
            user: user.map(Path::to_path_buf),
            homes: homes.cloned(),
            cwd: cwd.to_path_buf(),
        }
    }

    /// `[chat]`.
    pub fn chat(&self) -> Result<agent::Config> {
        self.part("chat", agent::Config::over)
    }

    /// `[agent]`.
    pub fn agent_table(&self) -> Result<agent::Table> {
        self.part("agent", agent::Table::over)
    }

    /// `[memory]`.
    pub fn memory(&self) -> Result<memory::Config> {
        self.part("memory", memory::Config::over)
    }

    /// `[summariser]`.
    pub fn summariser(&self) -> Result<summariser::Config> {
        self.part("summariser", summariser::Config::over)
    }

    /// `[agents.<name>]`, its paths absolute, except a relative `worktree` outside a project;
    /// `None` when no file has the table. Of the agents' tables, only this agent's are checked.
    pub fn agent(&self, name: &str) -> Result<Option<Agent>> {
        let (sources, home) = self.sources()?;
        let mut found: Option<Agent> = None;
        for s in &sources {
            let keys = s.de(Only {
                key: "agents",
                seed: Only {
                    key: name,
                    seed: PhantomData::<Keys>,
                },
            })?;
            if let Some(k) = keys.flatten() {
                let a = Agent {
                    instructions: k.instructions.iter().map(|p| s.dir.join(p)).collect(),
                    worktree: k.worktree,
                };
                found = Some(a.over(found.unwrap_or_default()));
            }
        }
        Ok(found.map(|a| match &home {
            Some(h) => a.under(h),
            None => a,
        }))
    }

    /// The agents' `reports-to`, each file's over those beneath it: a hierarchy with no cycle,
    /// each superior an agent with a table.
    pub fn hierarchy(&self) -> Result<Hierarchy> {
        let (sources, _) = self.sources()?;
        let mut ranks = BTreeMap::<String, Option<Vec<String>>>::new();
        for s in &sources {
            let agents: Option<BTreeMap<String, Rank>> = s.part("agents")?;
            for (name, r) in agents.unwrap_or_default() {
                let at = ranks.entry(name).or_default();
                if r.reports_to.is_some() {
                    *at = r.reports_to;
                }
            }
        }
        let h = Hierarchy {
            superiors: ranks
                .into_iter()
                .map(|(n, r)| (n, r.unwrap_or_default()))
                .collect(),
        };
        h.check()?;
        Ok(h)
    }

    /// Table `key` of every file, each over those beneath it by `over`.
    fn part<T: DeserializeOwned + Default>(&self, key: &str, over: fn(T, T) -> T) -> Result<T> {
        let (sources, _) = self.sources()?;
        sources.iter().try_fold(T::default(), |base, s| {
            Ok(match s.part(key)? {
                Some(t) => over(t, base),
                None => base,
            })
        })
    }

    /// The files that exist, lowest first: the user's, the nearest project's, its private one;
    /// and the project's [`home`](project::home), if in one.
    fn sources(&self) -> Result<(Vec<Source>, Option<PathBuf>)> {
        let user = self.user.as_deref();
        let project = nearest(user, &self.cwd);
        let home = match project.as_deref().and_then(Path::parent) {
            Some(dir) => Some(project::home(dir)?),
            None => None,
        };
        let private = match (project.as_deref(), &self.homes) {
            (Some(p), Some(homes)) => match Project::at(p)? {
                Some(project) => Some(project.private(homes)?),
                None => None,
            },
            _ => None,
        };
        if let (Some(h), Some(p)) = (&home, &private) {
            check_link(h, p)?;
        }
        let beside = |p: &Path| p.parent().unwrap_or(Path::new("")).to_path_buf();
        let sources = [
            user.map(|p| (p.to_path_buf(), beside(p), Layer::User)),
            project.map(|p| {
                let dir = beside(&p);
                (p, dir, Layer::Project)
            }),
            private.zip(home.clone()).map(|(p, h)| (p, h, Layer::Local)),
        ]
        .into_iter()
        .flatten()
        .filter_map(|(p, dir, layer)| Source::read(&p, dir, layer).transpose())
        .collect::<Result<Vec<_>>>()?;
        Ok((sources, home))
    }
}

/// The agents' superiors, as `reports-to` sets them: a graph with no cycle, each superior an
/// agent with a table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hierarchy {
    /// Every agent with a table, and the agents it reports to.
    superiors: BTreeMap<String, Vec<String>>,
}

impl Hierarchy {
    /// The agents `name` reports to; none when no file sets its `reports-to`.
    pub fn superiors(&self, name: &str) -> &[String] {
        self.superiors.get(name).map_or(&[], Vec::as_slice)
    }

    /// The agents below agent `name`: those that report to it, and every agent below them, through
    /// any number of levels. These are the threads `name` may read besides its own; its superiors
    /// and siblings are never among them.
    pub fn below(&self, name: &str) -> BTreeSet<&str> {
        let mut found = BTreeSet::new();
        let mut next = vec![name];
        while let Some(boss) = next.pop() {
            for (n, s) in &self.superiors {
                if s.iter().any(|s| s == boss) && found.insert(n.as_str()) {
                    next.push(n);
                }
            }
        }
        found
    }

    /// Refuses an agent that reports to itself, to an agent no table defines, or, through others,
    /// to an agent below it.
    fn check(&self) -> Result<()> {
        for (n, superiors) in &self.superiors {
            for s in superiors {
                if s == n {
                    bail!("Agent {n} reports to itself. Take {n} out of its reports-to.");
                }
                if !self.superiors.contains_key(s) {
                    bail!(
                        "Agent {n} reports to {s}, which no [agents.{s}] table defines. Add the \
                         table to an {FILE}, or take {s} out of {n}'s reports-to."
                    );
                }
            }
        }
        let mut done = BTreeSet::new();
        for n in self.superiors.keys() {
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
        for s in self.superiors(n) {
            if let Some(cycle) = self.cycle_from(s, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        done.insert(n);
        None
    }
}

/// Where a settings file sits, which decides whether it may hold `[project]`.
#[derive(Clone, Copy)]
enum Layer {
    User,
    Project,
    Local,
}
