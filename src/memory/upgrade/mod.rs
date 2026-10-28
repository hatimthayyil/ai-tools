mod v1;
mod v2;

use anyhow::Result;

use crate::{
    git::{self, Changes},
    memory::{store::Snapshot, tree::LEVELS},
};

/// The store format this ai reads and writes.
pub const FORMAT: u32 = 3;

/// The node segment columns of format 2, before `stale`.
const COLUMNS_V2: [&str; 6] = ["ts", "origin", "agent", "model", "session", "text"];

/// Where format 1 and 2 keep their one tree of nodes at level `l`.
fn legacy_level_dir(l: u32) -> String {
    format!("tree/{l}")
}

type Upgrade = fn(&Snapshot) -> Result<Changes>;

/// `UPGRADES[v - 1]` takes a tree of format `v` to format `v + 1`.
const UPGRADES: [Upgrade; FORMAT as usize - 1] = [v1::to_v2, v2::to_v3];

/// The changes taking `s`, a tree of format `v`, to format `v + 1`.
pub fn step(s: &Snapshot, v: u32) -> Result<Changes> {
    let mut ch = UPGRADES[v as usize - 1](s)?;
    ch.put(git::FORMAT.into(), git::format_blob(v + 1));
    Ok(ch)
}
