use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::{
    chat,
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
    pub chat: chat::Config,
    pub memory: memory::Config,
    pub summariser: summariser::Config,
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
/// key; unset keys keep their defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub chat: chat::Config,
    pub memory: memory::Config,
    pub summariser: summariser::Config,
}

impl Settings {
    /// The user's `ai.toml`, under the nearest one at or above `cwd`, under the `ai.local.toml`
    /// beside that.
    pub fn find(cwd: &Path) -> Result<Settings> {
        Settings::load(user_file().as_deref(), cwd)
    }

    /// [`Settings::find`] with the user's file at `user`.
    pub fn load(user: Option<&Path>, cwd: &Path) -> Result<Settings> {
        let project = nearest(user, cwd);
        let local = project.as_deref().map(|p| p.with_file_name(LOCAL));
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
            memory: f.memory,
            summariser: f.summariser,
        }))
    }

    fn over(self, base: Settings) -> Settings {
        Settings {
            chat: self.chat.over(base.chat),
            memory: self.memory.over(base.memory),
            summariser: self.summariser.over(base.summariser),
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
