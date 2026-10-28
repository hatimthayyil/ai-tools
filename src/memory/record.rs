use anyhow::Result;
use jiff::{civil::DateTime, tz::TimeZone};
use serde::{Deserialize, Serialize};

use crate::record;

const TS: &str = "%Y%m%dT%H%M%SZ";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Kind {
    User,
    Ai,
    Tool,
    Note,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::User, Kind::Ai, Kind::Tool, Kind::Note];

    pub fn name(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Ai => "ai",
            Kind::Tool => "tool",
            Kind::Note => "note",
        }
    }

    /// Whether this kind shows in the view: a tool call's line is empty, seen only by zooming.
    pub fn shown(self) -> bool {
        self != Kind::Tool
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub repo: String,
    pub project: String,
    pub head: String,
    pub branch: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Who {
    pub agent: String,
    pub model: String,
    pub session: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub ts: String,
    pub kind: Kind,
    pub origin: String,
    pub place: Place,
    pub who: Who,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub ts: String,
    pub origin: String,
    pub who: Who,
    /// Built from sources that have changed since: the text stays until it is rebuilt.
    pub stale: bool,
    pub text: String,
}

/// The columns this ai writes a node segment with; `text` is last. `stale` is `1` or empty.
pub const COLUMNS: [&str; 7] = ["ts", "origin", "agent", "model", "session", "stale", "text"];

/// A header or column value: whitespace becomes `_`.
fn field(v: &str) -> String {
    v.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// Newlines as single spaces: how every line shows a text.
pub fn flat(s: &str) -> String {
    s.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

pub fn valid_ts(ts: &str) -> bool {
    DateTime::strptime(TS, ts).is_ok()
}

impl Message {
    /// Unstamped: [`crate::memory::Store::append`] gives it the time it lands in the log.
    pub fn new(kind: Kind, text: &str) -> Message {
        Message {
            ts: String::new(),
            kind,
            origin: String::new(),
            place: Place::default(),
            who: Who::default(),
            text: text.into(),
        }
    }

    fn slot(&mut self, key: &str) -> Option<&mut String> {
        let (p, w) = (&mut self.place, &mut self.who);
        Some(match key {
            "ts" => &mut self.ts,
            "origin" => &mut self.origin,
            "repo" => &mut p.repo,
            "project" => &mut p.project,
            "head" => &mut p.head,
            "branch" => &mut p.branch,
            "agent" => &mut w.agent,
            "model" => &mut w.model,
            "session" => &mut w.session,
            _ => return None,
        })
    }

    /// `key value` lines, the empty ones left out, a blank line, then the text verbatim.
    pub fn encode(&self) -> Vec<u8> {
        let (p, w) = (&self.place, &self.who);
        let fields = [
            ("ts", self.ts.as_str()),
            ("kind", self.kind.name()),
            ("origin", &self.origin),
            ("repo", &p.repo),
            ("project", &p.project),
            ("head", &p.head),
            ("branch", &p.branch),
            ("agent", &w.agent),
            ("model", &w.model),
            ("session", &w.session),
        ];
        record::encode(fields.map(|(k, v)| (k, field(v))), &self.text)
    }

    /// Needs `ts`, `kind` and `origin`; other fields absent are empty, unknown ones ignored.
    pub fn decode(b: &[u8]) -> Result<Message> {
        let corrupt = || anyhow::anyhow!("A message is corrupt.");
        let (fields, text) = record::decode(b).ok_or_else(corrupt)?;
        let mut m = Message::new(Kind::Note, text);
        let mut kind = None;
        for (k, v) in fields {
            if k == "kind" {
                let new = Kind::parse(v).ok_or_else(corrupt)?;
                if kind.replace(new).is_some() {
                    return Err(corrupt());
                }
            } else if let Some(slot) = m.slot(k) {
                if !slot.is_empty() {
                    return Err(corrupt());
                }
                *slot = v.into();
            }
        }
        m.kind = kind.ok_or_else(corrupt)?;
        if !valid_ts(&m.ts) || m.origin.is_empty() {
            return Err(corrupt());
        }
        Ok(m)
    }

    /// The message as the tree sees it: a note bare, a chat message as `kind: text`.
    pub fn label(&self) -> String {
        match self.kind {
            Kind::Note => self.text.clone(),
            k => format!("{}: {}", k.name(), self.text),
        }
    }

    /// Its leaf: the flat label, or empty for a kind not [`Kind::shown`].
    pub fn line(&self) -> String {
        if self.kind.shown() {
            flat(&self.label())
        } else {
            String::new()
        }
    }

    /// `YYYY-MM-DD HH:MM` in the time zone `tz`.
    pub fn time_in(&self, tz: &TimeZone) -> String {
        let at = DateTime::strptime(TS, &self.ts)
            .and_then(|dt| dt.to_zoned(TimeZone::UTC))
            .expect("a decoded timestamp");
        at.with_time_zone(tz.clone())
            .strftime("%Y-%m-%d %H:%M")
            .to_string()
    }
}

impl Node {
    fn slot(&mut self, col: &str) -> Option<&mut String> {
        let w = &mut self.who;
        Some(match col {
            "ts" => &mut self.ts,
            "origin" => &mut self.origin,
            "agent" => &mut w.agent,
            "model" => &mut w.model,
            "session" => &mut w.session,
            "text" => &mut self.text,
            _ => return None,
        })
    }

    /// One line under the column row `cols`, the text flattened; a column it lacks is empty.
    pub fn encode(&self, cols: &[String]) -> String {
        let mut n = self.clone();
        n.text = flat(&self.text);
        let vs: Vec<String> = cols
            .iter()
            .map(|c| match (c.as_str(), n.slot(c)) {
                ("stale", _) => String::from(if self.stale { "1" } else { "" }),
                (_, Some(v)) if c == "text" => v.clone(),
                (_, Some(v)) => field(v),
                (_, None) => String::new(),
            })
            .collect();
        vs.join(" ")
    }

    /// The node on `line` under the column row `cols`; `None` if not built.
    pub fn decode(line: &str, cols: &[String]) -> Option<Node> {
        let mut n = Node::default();
        let mut vs = line.splitn(cols.len(), ' ');
        for c in cols {
            let v = vs.next()?;
            if c == "stale" {
                n.stale = v == "1";
            } else if let Some(slot) = n.slot(c) {
                *slot = v.into();
            }
        }
        valid_ts(&n.ts).then_some(n)
    }
}

pub fn now() -> String {
    ts(jiff::Timestamp::now().to_zoned(TimeZone::UTC).datetime())
}

fn ts(dt: DateTime) -> String {
    dt.strftime(TS).to_string()
}

pub fn midnight(date: jiff::civil::Date) -> String {
    ts(date.to_datetime(jiff::civil::Time::midnight()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Message {
        Message {
            ts: "20261003T142233Z".into(),
            kind: Kind::Tool,
            origin: "7KQ2ZD".into(),
            place: Place {
                repo: "acme/widget".into(),
                project: String::new(),
                head: "0123456789ab".into(),
                branch: "feature/a b".into(),
            },
            who: Who {
                agent: "claude-code".into(),
                model: "".into(),
                session: "ünï".into(),
            },
            text: "line one\n\nline two\n".into(),
        }
    }

    #[test]
    fn messages_round_trip() {
        let m = message();
        let b = m.encode();
        assert_eq!(
            std::str::from_utf8(&b).unwrap(),
            "ts 20261003T142233Z\nkind tool\norigin 7KQ2ZD\nrepo acme/widget\n\
             head 0123456789ab\nbranch feature/a_b\nagent claude-code\nsession ünï\n\n\
             line one\n\nline two\n"
        );
        let back = Message::decode(&b).unwrap();
        assert_eq!(back.place.branch, "feature/a_b");
        assert_eq!(
            (back.who.model.as_str(), back.place.project.as_str()),
            ("", "")
        );
        assert_eq!(back.text, m.text);
        assert_eq!(back.label(), "tool: line one\n\nline two\n");
        let note = Message {
            kind: Kind::Note,
            ..back.clone()
        };
        assert_eq!(note.label(), "line one\n\nline two\n");
        let tz = TimeZone::fixed(jiff::tz::offset(-15));
        assert_eq!(back.time_in(&tz), "2026-10-02 23:22");
        assert_eq!(back.time_in(&TimeZone::UTC), "2026-10-03 14:22");
        assert_eq!(Message::decode(&back.encode()).unwrap(), back);
    }

    #[test]
    fn message_headers_need_ts_kind_and_origin_and_skip_unknown_keys() {
        let ok = b"origin O\nlater a value\nkind note\nts 20261003T142233Z\n\nx";
        let m = Message::decode(ok).unwrap();
        assert_eq!(
            (m.kind, m.origin.as_str(), m.text.as_str()),
            (Kind::Note, "O", "x")
        );
        for bad in [
            &b"garbage"[..],
            b"ts 20261003T142233Z\nkind note\n\nx",
            b"ts 20261003T142233Z\norigin O\n\nx",
            b"kind note\norigin O\n\nx",
            b"ts 2026\nkind note\norigin O\n\nx",
            b"ts 20261003T142233Z\nkind chat\norigin O\n\nx",
            b"ts 20261003T142233Z\nkind note\norigin O\norigin P\n\nx",
            b"ts 20261003T142233Z\nkind note\norigin O\nrepo\n\nx",
            b"20261003T142233Z note O - - - - - -\nx",
            &[0xff, b'\n', b'\n'],
        ] {
            assert!(
                Message::decode(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    fn cols(cs: &[&str]) -> Vec<String> {
        cs.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn nodes_are_one_line_under_their_columns() {
        let n = Node {
            ts: "20261003T142233Z".into(),
            origin: "7KQ2ZD".into(),
            who: Who {
                agent: "claude-code".into(),
                model: "".into(),
                session: "s 1".into(),
            },
            stale: false,
            text: "a\nb\r\nc".into(),
        };
        let cs = cols(&COLUMNS);
        let line = n.encode(&cs);
        assert_eq!(line, "20261003T142233Z 7KQ2ZD claude-code  s_1  a b c");
        let stale = Node {
            stale: true,
            ..n.clone()
        };
        let line = stale.encode(&cs);
        assert_eq!(line, "20261003T142233Z 7KQ2ZD claude-code  s_1 1 a b c");
        let flat_text = Node {
            who: Who {
                session: "s_1".into(),
                ..n.who.clone()
            },
            text: "a b c".into(),
            ..stale
        };
        assert_eq!(Node::decode(&line, &cs), Some(flat_text));
        let back = Node::decode(&n.encode(&cs), &cs).unwrap();
        assert!(!back.stale);
        assert_eq!(
            (
                back.who.model.as_str(),
                back.who.session.as_str(),
                back.text.as_str()
            ),
            ("", "s_1", "a b c")
        );
        assert_eq!(Node::decode(&back.encode(&cs), &cs), Some(back.clone()));
        assert!(Node::decode("", &cs).is_none());
        assert_eq!(
            Node::decode("20261003T142233Z O     ", &cs).unwrap().text,
            ""
        );
        assert_eq!(
            Node::decode("20261003T142233Z O     ", &cs).unwrap().origin,
            "O"
        );
        let wider = cols(&["ts", "origin", "agent", "model", "session", "later", "text"]);
        let line = back.encode(&wider);
        assert_eq!(line, "20261003T142233Z 7KQ2ZD claude-code  s_1  a b c");
        assert_eq!(Node::decode(&line, &wider), Some(back));
        assert_eq!(Kind::parse("note"), Some(Kind::Note));
        assert_eq!(Kind::parse("work"), None);
    }
}
