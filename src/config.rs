use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use serde::Deserialize;

use crate::{
    chat,
    file::{AtPath, pretty},
    memory,
};

/// The settings file: the user's in [`home`], a project's at or above where `ai` runs.
pub const FILE: &str = "ai.toml";

/// `~/.ai`: the user's settings, and every store not pointed elsewhere.
pub fn home() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(".ai"))
}

/// What `ai.toml` sets: the project's file over the user's, key by key; unset keys keep their
/// defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub chat: chat::Config,
    pub memory: memory::Config,
}

impl Settings {
    /// The user's `ai.toml`, under the nearest one at or above `cwd`.
    pub fn find(cwd: &Path) -> Result<Settings> {
        Settings::load(home().map(|h| h.join(FILE)).as_deref(), cwd)
    }

    /// [`Settings::find`] with the user's file at `user`.
    pub fn load(user: Option<&Path>, cwd: &Path) -> Result<Settings> {
        let user = user.map(Settings::read).transpose()?.flatten();
        let project = cwd
            .ancestors()
            .map(|d| d.join(FILE))
            .find(|p| p.is_file())
            .map(|p| Settings::read(&p))
            .transpose()?
            .flatten();
        Ok(project.unwrap_or_default().over(user.unwrap_or_default()))
    }

    /// The settings in `p`, `None` without the file; a bad key or value names both.
    fn read(p: &Path) -> Result<Option<Settings>> {
        let text = match fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).at(p),
        };
        toml::from_str(&text)
            .map(Some)
            .map_err(|e| anyhow!("{}: {e}", pretty(p)))
    }

    fn over(self, base: Settings) -> Settings {
        Settings {
            chat: self.chat.over(base.chat),
            memory: self.memory.over(base.memory),
        }
    }
}
