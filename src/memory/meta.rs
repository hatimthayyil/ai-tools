use std::collections::HashMap;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::memory::{record::Message, store::Snapshot, tree::Coord};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Field {
    Origin,
    Repo,
    Root,
    Head,
    Branch,
    Agent,
    Model,
    Session,
    Kind,
}

impl Field {
    pub fn name(self) -> &'static str {
        match self {
            Field::Origin => "origin",
            Field::Repo => "repo",
            Field::Root => "root",
            Field::Head => "head",
            Field::Branch => "branch",
            Field::Agent => "agent",
            Field::Model => "model",
            Field::Session => "session",
            Field::Kind => "kind",
        }
    }

    fn of(self, m: &Message) -> &str {
        match self {
            Field::Origin => &m.origin,
            Field::Repo => &m.place.repo,
            Field::Root => &m.place.root,
            Field::Head => &m.place.head,
            Field::Branch => &m.place.branch,
            Field::Agent => &m.who.agent,
            Field::Model => &m.who.model,
            Field::Session => &m.who.session,
            Field::Kind => m.kind.name(),
        }
    }
}

/// What a line shows of its messages' metadata, between its `id+n|` and its text.
#[derive(Args, Clone, Debug, Default)]
pub struct Meta {
    #[arg(long, help = "add the time of day (UTC) to the dates")]
    pub time: bool,
    #[arg(
        short = 'o',
        long = "fields",
        value_enum,
        value_delimiter = ',',
        value_name = "FIELD,...",
        help = "add these fields; mixed values show as the commonest and (+k) others"
    )]
    pub fields: Vec<Field>,
}

/// How an empty field shows.
pub fn dash(v: &str) -> &str {
    if v.is_empty() { "-" } else { v }
}

pub fn date(ts: &str) -> String {
    format!("{}-{}-{}", &ts[..4], &ts[4..6], &ts[6..8])
}

fn time(ts: &str) -> String {
    format!("{}:{}", &ts[9..11], &ts[11..13])
}

/// `2026-10-08 09:12`, UTC.
pub fn stamp(ts: &str) -> String {
    format!("{} {}", date(ts), time(ts))
}

/// What a line shows of `a..b` under the header of `a`'s day, UTC: nothing within the day,
/// else `..10-05`; with `time`, `09:12`, `09:12..17:40` or `09:12..10-05 17:40`. The end
/// drops the year it shares with the start.
pub fn range(a: &str, b: &str, time_of_day: bool) -> Option<String> {
    let same_day = a[..8] == b[..8];
    let day = match a[..4] == b[..4] {
        true => date(b)[5..].to_string(),
        false => date(b),
    };
    if !time_of_day {
        return (!same_day).then(|| format!("..{day}"));
    }
    let (from, to) = (time(a), time(b));
    Some(match (same_day, from == to) {
        (true, true) => from,
        (true, false) => format!("{from}..{to}"),
        (false, _) => format!("{from}..{day} {to}"),
    })
}

/// The one value, else the commonest (a real one before `-`, the earliest of a tie) and
/// `(+k)` for the k others.
fn values<'a>(vs: impl Iterator<Item = &'a str>) -> String {
    let mut count: HashMap<&str, (usize, usize)> = HashMap::new();
    for (k, v) in vs.enumerate() {
        count.entry(v).or_insert((0, k)).0 += 1;
    }
    let top = count
        .iter()
        .max_by_key(|(v, (n, first))| (**v != "-", *n, std::cmp::Reverse(*first)))
        .map_or("-", |(v, _)| v);
    match count.len() {
        0 | 1 => top.into(),
        n => format!("{top}(+{})", n - 1),
    }
}

/// A printed line `id+n|range|field|…|text`, under the header of `day`, the day it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub c: Coord,
    pub day: String,
    pub text: String,
}

impl Line {
    /// Bytes printed for it, its day's header too when `header`.
    pub fn size(&self, header: bool) -> u64 {
        let h = if header { self.day.len() + 1 } else { 0 };
        (self.text.len() + 1 + h) as u64
    }
}

/// `lines` with a date header before the first and wherever the day changes.
pub fn dated(lines: &[Line]) -> Vec<String> {
    let mut out = Vec::new();
    let mut day = None;
    for l in lines {
        if day != Some(&l.day) {
            out.push(l.day.clone());
            day = Some(&l.day);
        }
        out.push(l.text.clone());
    }
    out
}

impl Meta {
    /// Line `c` showing `text`, over `ms`, the messages it covers in order. Only the first
    /// and last are read for the dates: the log is in time order.
    pub fn line(&self, c: Coord, ms: &[Message], text: &str) -> Line {
        let (a, b) = (&ms[0], &ms[ms.len() - 1]);
        let mut s = format!("{c}|");
        let fields = self
            .fields
            .iter()
            .map(|f| values(ms.iter().map(|m| dash(f.of(m)))));
        for col in range(&a.ts, &b.ts, self.time).into_iter().chain(fields) {
            s += &col;
            s.push('|');
        }
        s += text;
        Line {
            c,
            day: date(&a.ts),
            text: s,
        }
    }

    /// [`Meta::line`] of `c`, reading only the messages it needs.
    pub fn line_at(&self, s: &Snapshot, c: Coord, text: &str) -> Result<Line> {
        let mut ids: Vec<u64> = match self.fields.is_empty() {
            true => vec![c.id(), c.end() - 1],
            false => (c.id()..c.end()).collect(),
        };
        ids.dedup();
        let ms = ids
            .into_iter()
            .map(|i| s.message(i))
            .collect::<Result<Vec<_>>>()?;
        Ok(self.line(c, &ms, text))
    }

    /// The flags that ask for this, as typed.
    pub fn args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if self.time {
            a.push("--time".into());
        }
        if !self.fields.is_empty() {
            let names: Vec<_> = self.fields.iter().map(|f| f.name()).collect();
            a.extend(["-o".into(), names.join(",")]);
        }
        a
    }

    /// [`Meta::args`], each after a space.
    pub fn flags(&self) -> String {
        self.args().iter().map(|a| format!(" {a}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_show_what_the_header_does_not() {
        let (a, b) = ("20261004T091200Z", "20261005T174000Z");
        assert_eq!(range(a, b, false).as_deref(), Some("..10-05"));
        assert_eq!(range(a, b, true).as_deref(), Some("09:12..10-05 17:40"));
        assert_eq!(range(a, "20261004T235959Z", false), None);
        assert_eq!(
            range(a, "20261004T174000Z", true).as_deref(),
            Some("09:12..17:40")
        );
        assert_eq!(range(a, "20261004T091259Z", true).as_deref(), Some("09:12"));
        assert_eq!(
            range("20251230T000000Z", "20260102T000000Z", false).as_deref(),
            Some("..2026-01-02")
        );
        assert_eq!(
            range("20251231T235000Z", "20260101T001000Z", true).as_deref(),
            Some("23:50..2026-01-01 00:10")
        );
    }

    fn line(c: Coord, day: &str) -> Line {
        Line {
            c,
            day: day.into(),
            text: format!("{c}|x"),
        }
    }

    #[test]
    fn headers_mark_each_new_day() {
        let (a, b) = ("2026-10-04", "2026-10-05");
        let ls = vec![
            line(Coord::new(3, 0), a),
            line(Coord::leaf(8), a),
            line(Coord::leaf(9), b),
            line(Coord::leaf(10), b),
        ];
        assert_eq!(dated(&ls), [a, "0+8|x", "8|x", b, "9|x", "10|x"]);
    }

    #[test]
    fn many_values_show_the_commonest_and_a_count() {
        assert_eq!(values(["a"].into_iter()), "a");
        assert_eq!(values(["a", "a"].into_iter()), "a");
        assert_eq!(values(["a", "b", "b", "c"].into_iter()), "b(+2)");
        assert_eq!(values(["a", "b"].into_iter()), "a(+1)");
        assert_eq!(values(["-", "-", "a"].into_iter()), "a(+1)");
    }
}
