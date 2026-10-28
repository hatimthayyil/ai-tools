//! Format 1: one positional header line per message, `-` for an empty field; node
//! segments without a column row.

use anyhow::{Context, Result, anyhow};

use super::{COLUMNS_V2, LEVELS, legacy_level_dir};
use crate::{
    git::{Changes, fan_path},
    memory::{
        record::{Kind, Message, Node, Place, Who, valid_ts},
        store::{LOG, Snapshot},
    },
    record::Seg,
};

fn value(v: &str) -> String {
    if v == "-" { String::new() } else { v.into() }
}

fn message(b: &[u8]) -> Result<Message> {
    let corrupt = || anyhow!("A message is corrupt.");
    let s = std::str::from_utf8(b).map_err(|_| corrupt())?;
    let (head, text) = s.split_once('\n').ok_or_else(corrupt)?;
    let f: Vec<&str> = head.split(' ').collect();
    let [ts, kind, origin, repo, hd, branch, agent, model, session] = f[..] else {
        return Err(corrupt());
    };
    if !valid_ts(ts) {
        return Err(corrupt());
    }
    Ok(Message {
        ts: ts.into(),
        kind: Kind::parse(kind).ok_or_else(corrupt)?,
        origin: value(origin),
        place: Place {
            repo: value(repo),
            project: String::new(),
            head: value(hd),
            branch: value(branch),
        },
        who: Who {
            agent: value(agent),
            model: value(model),
            session: value(session),
        },
        text: text.into(),
    })
}

fn node(line: &str) -> Option<Node> {
    let mut f = line.splitn(6, ' ');
    let [ts, origin, agent, model, session, text] = [(); 6].map(|_| f.next().unwrap_or(""));
    valid_ts(ts).then(|| Node {
        ts: ts.into(),
        origin: value(origin),
        who: Who {
            agent: value(agent),
            model: value(model),
            session: value(session),
        },
        text: text.into(),
        ..Node::default()
    })
}

/// Every message and segment re-encoded in place; ids and texts unchanged.
pub fn to_v2(s: &Snapshot) -> Result<Changes> {
    let mut ch = Changes::default();
    s.walk(LOG, |i, id| {
        let m = message(&s.fetch(id)?).with_context(|| format!("Message {i}"))?;
        ch.put(fan_path(LOG, i), m.encode());
        Ok(())
    })?;
    for l in 0..LEVELS {
        let dir = legacy_level_dir(l);
        s.walk(&dir, |k, id| {
            let b = s.fetch(id)?;
            let text =
                std::str::from_utf8(&b).map_err(|_| anyhow!("A tree segment is corrupt."))?;
            let mut seg = Seg::new(&COLUMNS_V2);
            seg.lines = text
                .strip_suffix('\n')
                .unwrap_or(text)
                .split('\n')
                .map(|line| node(line).map_or_else(String::new, |n| n.encode(&seg.cols)))
                .collect();
            ch.put(fan_path(&dir, k), seg.bytes());
            Ok(())
        })?;
    }
    Ok(ch)
}
