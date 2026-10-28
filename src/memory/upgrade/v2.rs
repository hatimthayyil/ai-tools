//! Format 2: one tree of nodes at `tree/<level>`, with no `stale` column. Format 3: a tree per
//! number at `trees/<n>/<level>`, `stale` on every node, and the `state` naming the current one.

use anyhow::{Result, anyhow};

use super::{COLUMNS_V2, LEVELS, legacy_level_dir};
use crate::{
    git::{Changes, fan_path},
    memory::{
        record::{COLUMNS, Node},
        store::{STATE, Snapshot, State, level_dir},
    },
    record::Seg,
};

/// Moves the one tree of format 2 to tree 1, which is then the current one.
pub fn to_v3(s: &Snapshot) -> Result<Changes> {
    let mut ch = Changes::default();
    for l in 0..LEVELS {
        let dir = legacy_level_dir(l);
        s.walk(&dir, |k, id| {
            let b = s.fetch(id)?;
            let seg =
                Seg::parse(&b, &COLUMNS_V2).ok_or_else(|| anyhow!("A tree segment is corrupt."))?;
            let mut out = Seg::new(&COLUMNS);
            out.lines = seg
                .lines
                .iter()
                .map(|line| {
                    Node::decode(line, &seg.cols).map_or_else(String::new, |n| n.encode(&out.cols))
                })
                .collect();
            ch.put(fan_path(&level_dir(1, l), k), out.bytes());
            Ok(())
        })?;
    }
    ch.remove("tree".into());
    ch.put(STATE.into(), State::default().bytes());
    Ok(ch)
}
