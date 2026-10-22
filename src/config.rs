use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::{
    chat,
    file::{AtPath, pretty},
    memory, project,
};

/// The settings file: the user's in [`home`], a project's at or above where `ai` runs.
pub const FILE: &str = "ai.toml";

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

/// What `ai.toml` sets: the project's file over the user's, key by key; unset keys keep their
/// defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub chat: chat::Config,
    pub memory: memory::Config,
}

impl Settings {
    /// The user's `ai.toml`, under the nearest one at or above `cwd`.
    pub fn find(cwd: &Path) -> Result<Settings> {
        Settings::load(user_file().as_deref(), cwd)
    }

    /// [`Settings::find`] with the user's file at `user`.
    pub fn load(user: Option<&Path>, cwd: &Path) -> Result<Settings> {
        let mine = user.map(|p| Settings::read(p, true)).transpose()?.flatten();
        let project = nearest(user, cwd)
            .map(|p| Settings::read(&p, false))
            .transpose()?
            .flatten();
        Ok(project.unwrap_or_default().over(mine.unwrap_or_default()))
    }

    /// The settings in `p`, `None` without the file; the user's file holds no `[project]`.
    fn read(p: &Path, user: bool) -> Result<Option<Settings>> {
        let Some(f) = read(p)? else {
            return Ok(None);
        };
        if user && f.project.is_some() {
            bail!(
                "{}: [project] belongs in a project's {FILE}, not in your own.",
                pretty(p)
            );
        }
        Ok(Some(Settings {
            chat: f.chat,
            memory: f.memory,
        }))
    }

    fn over(self, base: Settings) -> Settings {
        Settings {
            chat: self.chat.over(base.chat),
            memory: self.memory.over(base.memory),
        }
    }
}
