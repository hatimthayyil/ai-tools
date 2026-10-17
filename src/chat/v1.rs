use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use crate::{
    chat::{
        model::{Kind, Marker},
        record::{Agent, Binding, Head, Line, LineKind, encode},
    },
    git::{Changes, Snap, fan_path, format_blob},
    memory::Who,
    record::{self, Seg},
};

const COLUMNS: [&str; 5] = ["ts", "harness", "session", "entry", "parent"];

pub(super) fn export(source: &Snap) -> Result<Changes> {
    let mut changes = Changes::default();
    sequence(source, "", &mut changes)?;
    for (harness, _) in source.names("sessions")? {
        for (session, id) in source.names(&format!("sessions/{harness}"))? {
            changes.put(
                format!("sessions/{harness}/{session}"),
                encode(&binding(&harness, &session, &source.fetch(id)?)?)?,
            );
        }
    }
    for (harness, _) in source.names("agents")? {
        for (agent, _) in source.names(&format!("agents/{harness}"))? {
            let dir = format!("agents/{harness}/{agent}/");
            sequence(source, &dir, &mut changes)?;
            let path = format!("{dir}agent");
            changes.put(
                path.clone(),
                encode(&agent_record(&harness, &agent, &source.read(&path)?)?)?,
            );
        }
    }
    for (level, _) in source.names("lines")? {
        source.walk(&format!("lines/{level}"), |last, id| {
            changes.put(
                fan_path(&format!("lines/{level}"), last),
                encode(&line(&source.fetch(id)?)?)?,
            );
            Ok(())
        })?;
    }
    changes.put("format".into(), format_blob(super::store::FORMAT));
    Ok(changes)
}

fn sequence(source: &Snap, dir: &str, changes: &mut Changes) -> Result<()> {
    let mut next = 0;
    source.walk(&format!("{dir}index"), |segment, id| {
        let bytes = source.fetch(id)?;
        let seg = Seg::parse(&bytes, &COLUMNS).context("An index segment is corrupt.")?;
        if COLUMNS
            .iter()
            .any(|column| !seg.cols.iter().any(|actual| actual == column))
        {
            bail!("An index segment is corrupt.");
        }
        if segment * 256 != next || seg.lines.len() > 256 {
            bail!("An index segment is corrupt.");
        }
        let heads = seg
            .lines
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let seq = segment * 256 + offset as u64;
                let fields: HashMap<_, _> = seg.values(row).collect();
                let text = |key| fields.get(key).copied().unwrap_or("").to_string();
                let parent = number(fields.get("parent"))?;
                if parent.is_some_and(|parent| parent >= seq) {
                    bail!("Item {seq} has an invalid parent.");
                }
                Ok(Head {
                    seq,
                    parent,
                    harness: text("harness"),
                    session: text("session"),
                    entry: text("entry"),
                    ts: timestamp(fields.get("ts"))?.context("no time")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        next += heads.len() as u64;
        changes.put(fan_path(&format!("{dir}index"), segment), encode(&heads)?);
        Ok(())
    })?;
    let mut items = 0;
    source.walk(&format!("{dir}items"), |seq, id| {
        if seq != items || seq >= next {
            bail!("The v1 index and item sequence do not match.");
        }
        changes.put(
            fan_path(&format!("{dir}items"), seq),
            encode(&kind(&source.fetch(id)?)?)?,
        );
        items += 1;
        Ok(())
    })?;
    let mut raw = 0;
    source.walk(&format!("{dir}raw"), |seq, _| {
        if seq != raw || seq >= next {
            bail!("The v1 index and raw sequence do not match.");
        }
        raw += 1;
        Ok(())
    })?;
    if items != next || raw != next {
        bail!("The v1 index, items and raw sequences have different lengths.");
    }
    Ok(())
}

fn fields(bytes: &[u8]) -> Result<(HashMap<&str, &str>, &str)> {
    let (fields, body) = record::decode(bytes).context("A v1 record is corrupt.")?;
    Ok((fields.into_iter().collect(), body))
}

fn timestamp(value: Option<&&str>) -> Result<Option<Timestamp>> {
    value
        .map(|value| value.parse().context("a bad time"))
        .transpose()
}

fn number(value: Option<&&str>) -> Result<Option<u64>> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| value.parse().context("a bad item number"))
        .transpose()
}

fn kind(bytes: &[u8]) -> Result<Kind> {
    let (fields, body) = fields(bytes)?;
    let text = |key| fields.get(key).copied().unwrap_or("").to_string();
    Ok(match fields.get("kind").copied() {
        Some("user") => Kind::User(body.into()),
        Some("work") => Kind::Work {
            from: text("from"),
            text: body.into(),
        },
        Some("text") => Kind::Text(body.into()),
        Some("call") => Kind::ToolCall {
            id: text("call"),
            name: text("name"),
            input: serde_json::from_str(body)?,
        },
        Some("result") => Kind::ToolResult {
            id: text("call"),
            output: body.into(),
            is_error: fields.get("error") == Some(&"true"),
        },
        Some("marker") => Kind::Marker(match fields.get("marker").copied() {
            Some("compaction") => Marker::Compaction,
            Some("summary") => Marker::Summary,
            Some("interrupt") => Marker::Interrupt,
            _ => bail!("an unknown marker"),
        }),
        _ => bail!("an unknown kind"),
    })
}

fn binding(harness: &str, session: &str, bytes: &[u8]) -> Result<Binding> {
    let (fields, _) = fields(bytes)?;
    Ok(Binding {
        harness: harness.into(),
        session: session.into(),
        transcript: fields.get("transcript").map(PathBuf::from),
        graft: number(fields.get("graft"))?,
        tip: number(fields.get("tip"))?,
        attached: timestamp(fields.get("attached"))?.context("no attach time")?,
        detached: timestamp(fields.get("detached"))?,
    })
}

fn agent_record(harness: &str, agent: &str, bytes: &[u8]) -> Result<Agent> {
    let (fields, _) = fields(bytes)?;
    Ok(Agent {
        harness: harness.into(),
        agent: agent.into(),
        agent_type: fields.get("type").copied().unwrap_or("").into(),
        description: fields.get("description").copied().unwrap_or("").into(),
        call: number(fields.get("call"))?,
    })
}

fn line(bytes: &[u8]) -> Result<Line> {
    let (fields, body) = fields(bytes)?;
    let text = |key| fields.get(key).copied().unwrap_or("").to_string();
    Ok(Line {
        ts: timestamp(fields.get("ts"))?.context("no time")?,
        kind: match fields.get("kind").copied() {
            Some("chunk") => LineKind::Chunk,
            Some("exchange") => LineKind::Turn,
            Some("node") => LineKind::Node,
            _ => bail!("an unknown kind"),
        },
        who: Who {
            agent: text("agent"),
            model: text("model"),
            session: text("session"),
        },
        text: body.into(),
    })
}
