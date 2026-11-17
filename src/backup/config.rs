//! `[backup]`, in the user's own `ai.toml` alone: its keys name where every harness's files go
//! and the commands that print passwords, which no project's settings may choose.
//!
//! ```toml
//! [backup]
//! repository = "~/.ai/backup"
//! password-command = "cat /run/secrets/ai-backup"
//! every = "5m"
//! on-session-end = true
//! folders = ["~/.claude-work/projects"]
//!
//! [backup.remote]
//! repository = "s3:https://s3.example.com/bucket/ai"
//! password-command = "pass show backup/ai"
//! env-command = "pass show backup/ai-env"
//! ```

use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use jiff::SignedDuration;
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::{
    config::{self, FILE, Homes},
    error,
    file::pretty,
};

/// The local repository's folder under `~/.ai`, with `repository` unset.
pub const DIR: &str = "backup";
/// The time `ai backup watch` waits between two runs, with `every` unset.
pub const EVERY: Duration = Duration::from_secs(5 * 60);

/// `[backup]` as the file holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Table {
    #[serde(deserialize_with = "optional")]
    repository: Option<String>,
    #[serde(deserialize_with = "optional")]
    password_command: Option<String>,
    #[serde(deserialize_with = "every")]
    every: Option<Duration>,
    on_session_end: Option<bool>,
    folders: Vec<String>,
    remote: Option<Remote>,
}

/// `[backup.remote]`: a second repository, which gets a copy of every snapshot of the local one.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Remote {
    /// The repository as restic names it, such as `s3:https://host/bucket/path`.
    #[serde(deserialize_with = "required")]
    pub repository: String,
    /// Prints the repository's password; `sh -c` runs it.
    #[serde(deserialize_with = "required")]
    pub password_command: String,
    /// Prints `NAME=value` lines, the variables restic reaches the repository with, such as
    /// `AWS_ACCESS_KEY_ID`; `sh -c` runs it.
    #[serde(default, deserialize_with = "optional")]
    pub env_command: Option<String>,
}

/// `[backup]` with its defaults in and its paths whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The local repository's folder.
    pub repository: PathBuf,
    /// Prints the local repository's password; unset, the password is the key file's.
    pub password_command: Option<String>,
    pub every: Duration,
    /// Whether a session's end starts a run.
    pub on_session_end: bool,
    /// Folders backed up besides the harnesses' own.
    pub folders: Vec<PathBuf>,
    pub remote: Option<Remote>,
}

impl Config {
    /// `[backup]` of the user's `ai.toml` in `~/.ai`; a bad key or value is an error naming the
    /// file.
    pub fn load(homes: &Homes) -> Result<Config> {
        let file = homes.machine.join(FILE);
        config::backup(&file)?
            .resolve(homes)
            .map_err(|e| anyhow!("{}: [backup] {}", pretty(&file), error::line(&e)))
    }
}

impl Table {
    fn resolve(self, homes: &Homes) -> Result<Config> {
        let path = |key: &str, v: &str| {
            expand(v, &homes.home).map_err(|e| anyhow!("{key}: {}", error::line(&e)))
        };
        Ok(Config {
            repository: match &self.repository {
                Some(r) => path("repository", r)?,
                None => homes.machine.join(DIR),
            },
            password_command: self.password_command,
            every: self.every.unwrap_or(EVERY),
            on_session_end: self.on_session_end.unwrap_or(true),
            folders: self
                .folders
                .iter()
                .map(|f| path("folders", f))
                .collect::<Result<_>>()?,
            remote: self.remote,
        })
    }
}

/// `v` as a path: absolute, or `~` and what follows it under `home`. `.` steps are dropped;
/// `..` is refused, as it means another folder once a link is followed.
fn expand(v: &str, home: &Path) -> Result<PathBuf> {
    let raw = if v == "~" {
        home.to_path_buf()
    } else if let Some(rest) = v.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(v).is_absolute() {
        PathBuf::from(v)
    } else {
        bail!("'{v}' is neither an absolute path nor one under ~/.");
    };
    if raw.components().any(|c| c == Component::ParentDir) {
        bail!("'{v}' holds '..': name the folder without it.");
    }
    Ok(raw
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect())
}

fn required<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match String::deserialize(d)?.trim() {
        "" => Err(D::Error::custom("is empty")),
        s => Ok(s.into()),
    }
}

fn optional<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    required(d).map(Some)
}

/// A time of a second or more, as jiff reads one: `5m`, `1h 30m`, `90s`.
fn every<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let given = String::deserialize(d)?;
    let no = || {
        D::Error::custom(format!(
            "`{given}` is no time between runs: give a second or more, such as 5m or 1h"
        ))
    };
    let span: SignedDuration = given.trim().parse().map_err(|_| no())?;
    match Duration::try_from(span) {
        Ok(d) if d >= Duration::from_secs(1) => Ok(Some(d)),
        _ => Err(no()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn homes() -> Homes {
        Homes::at(Path::new("/home/u"))
    }

    fn config(text: &str) -> Result<Config> {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            backup: Table,
        }
        toml::from_str::<File>(text)?.backup.resolve(&homes())
    }

    fn error(text: &str) -> String {
        crate::error::line(&config(text).unwrap_err())
    }

    #[test]
    fn an_empty_table_is_the_defaults() {
        let c = config("").unwrap();
        assert_eq!(
            c,
            Config {
                repository: "/home/u/.ai/backup".into(),
                password_command: None,
                every: Duration::from_secs(300),
                on_session_end: true,
                folders: vec![],
                remote: None,
            }
        );
        assert_eq!(config("[backup]\n").unwrap(), c);
    }

    #[test]
    fn every_key_is_read() {
        let c = config(
            r#"
            [backup]
            repository = "~/b"
            password-command = "cat /k"
            every = "1h 30m"
            on-session-end = false
            folders = ["~/.claude-work/projects", "/srv/./x/"]
            [backup.remote]
            repository = "s3:https://h/b"
            password-command = "pass show r"
            env-command = "pass show e"
            "#,
        )
        .unwrap();
        assert_eq!(c.repository, PathBuf::from("/home/u/b"));
        assert_eq!(c.password_command.as_deref(), Some("cat /k"));
        assert_eq!(c.every, Duration::from_secs(5400));
        assert!(!c.on_session_end);
        assert_eq!(
            c.folders,
            [
                PathBuf::from("/home/u/.claude-work/projects"),
                "/srv/x".into()
            ]
        );
        assert_eq!(
            c.remote,
            Some(Remote {
                repository: "s3:https://h/b".into(),
                password_command: "pass show r".into(),
                env_command: Some("pass show e".into()),
            })
        );
    }

    #[test]
    fn a_bad_value_is_refused_naming_its_key() {
        assert!(error("[backup]\nevery = \"0s\"\n").contains("`0s` is no time between runs"));
        assert!(error("[backup]\nevery = \"soon\"\n").contains("no time between runs"));
        assert!(error("[backup]\nevery = \"-5m\"\n").contains("no time between runs"));
        assert!(error("[backup]\nevery = 5\n").contains("every"));
        assert!(error("[backup]\nrepository = \"\"\n").contains("is empty"));
        assert!(
            error("[backup]\nrepository = \"b\"\n")
                .contains("repository: 'b' is neither an absolute path nor one under ~/.")
        );
        assert!(error("[backup]\nfolders = [\"~/a/../b\"]\n").contains("folders: '~/a/../b'"));
        assert!(error("[backup]\nretention = 3\n").contains("unknown field"));
        assert!(
            error("[backup.remote]\nrepository = \"s3:x\"\n").contains("password-command"),
            "a remote needs its password"
        );
        assert!(
            error("[backup.remote]\nrepository = \" \"\npassword-command = \"x\"\n")
                .contains("is empty")
        );
    }
}
