use serde::{Deserialize, Deserializer, de::Error};

use crate::memory::tree::NODE;

/// The longest note: one node, so it is its own line of the view.
pub const NOTE_MAX: u64 = NODE as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knob {
    ViewLines,
    EntryChars,
}

impl Knob {
    pub const ALL: [Knob; 2] = [Knob::ViewLines, Knob::EntryChars];

    /// Its key under `[memory]` in `ai.toml`.
    pub fn key(self) -> &'static str {
        match self {
            Knob::ViewLines => "view-lines",
            Knob::EntryChars => "entry-chars",
        }
    }

    pub fn default(self) -> u64 {
        match self {
            Knob::ViewLines => 96,
            Knob::EntryChars => 280,
        }
    }

    pub fn what(self) -> &'static str {
        match self {
            Knob::ViewLines => "how many lines view prints (96 is about 8k tokens)",
            Knob::EntryChars => "the longest one note may be, in bytes",
        }
    }

    fn check(self, n: u64) -> Result<u64, String> {
        if n == 0 {
            return Err("must be a positive whole number".into());
        }
        if self == Knob::EntryChars && n > NOTE_MAX {
            return Err(format!(
                "is at most {NOTE_MAX}: a note has to fit one line of the view"
            ));
        }
        Ok(n)
    }

    fn read<'de, D: Deserializer<'de>>(self, d: D) -> Result<Option<u64>, D::Error> {
        self.check(u64::deserialize(d)?)
            .map(Some)
            .map_err(D::Error::custom)
    }
}

/// `[memory]` in `ai.toml`: the sizes it sets; every other knob follows the tool's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    #[serde(deserialize_with = "view_lines")]
    pub view_lines: Option<u64>,
    #[serde(deserialize_with = "entry_chars")]
    pub entry_chars: Option<u64>,
}

fn view_lines<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Knob::ViewLines.read(d)
}

fn entry_chars<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Knob::EntryChars.read(d)
}

impl Config {
    pub fn get(&self, k: Knob) -> u64 {
        self.overridden(k).unwrap_or(k.default())
    }

    pub fn overridden(&self, k: Knob) -> Option<u64> {
        match k {
            Knob::ViewLines => self.view_lines,
            Knob::EntryChars => self.entry_chars,
        }
    }

    /// These sizes, then `base`'s for those unset here.
    pub fn over(self, base: Config) -> Config {
        Config {
            view_lines: self.view_lines.or(base.view_lines),
            entry_chars: self.entry_chars.or(base.entry_chars),
        }
    }
}
