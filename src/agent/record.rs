use std::{fmt, path::PathBuf};

use anyhow::{Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    agent::model::{Kind, SourceId},
    memory::Who,
};

/// An `Option` field that must be present, as `null` when empty.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub seq: u64,
    #[serde(deserialize_with = "present")]
    pub parent: Option<u64>,
    pub harness: String,
    pub session: String,
    pub entry: String,
    pub ts: Timestamp,
}

/// One line of an item sequence: an item's head, its kind, and `ends_turn` only when true.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Row {
    #[serde(flatten)]
    pub head: Head,
    #[serde(flatten)]
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ends_turn: bool,
}

/// An item row's kind tag alone, its head and content skipped: whether the item opens a turn
/// (`Kind::opens`).
#[derive(Deserialize)]
pub(super) struct Tag {
    kind: Opener,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Opener {
    User,
    Work,
    #[serde(other)]
    Other,
}

impl Tag {
    pub(super) fn opens(&self) -> bool {
        !matches!(self.kind, Opener::Other)
    }
}

/// A session's branch in one thread: one line of the thread's `sessions.jsonl`, the last per
/// session holding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Track {
    pub harness: String,
    pub session: String,
    #[serde(deserialize_with = "present")]
    pub transcript: Option<PathBuf>,
    #[serde(deserialize_with = "present")]
    pub graft: Option<u64>,
    #[serde(deserialize_with = "present")]
    pub tip: Option<u64>,
}

/// An attachment change: one line of the project's `sessions.jsonl`, the last per session
/// holding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Event {
    pub harness: String,
    pub session: String,
    pub at: Timestamp,
    #[serde(flatten)]
    pub state: State,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum State {
    Attached { thread: String },
    Detached,
}

/// A session's place in one thread: its [`Track`] and when it was attached and detached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub harness: String,
    pub session: String,
    pub transcript: Option<PathBuf>,
    pub graft: Option<u64>,
    pub tip: Option<u64>,
    pub attached: Timestamp,
    pub detached: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub harness: String,
    pub agent: String,
    pub agent_type: String,
    pub description: String,
    #[serde(deserialize_with = "present")]
    pub call: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    pub level: u32,
    pub last: u64,
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}@{}", self.level, self.last)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Chunk,
    Turn,
    Node,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub ts: Timestamp,
    pub kind: LineKind,
    pub who: Who,
    pub text: String,
}

/// One line of a thread's `lines.jsonl`: lines fold in order, a written key keeping its first
/// line until dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum LineOp {
    Put { key: Key, line: Line },
    Drop { keys: Vec<Key> },
    Clear,
}

pub fn check_id<'s>(s: &'s str, what: &str) -> Result<&'s str> {
    let ok = !s.is_empty()
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !ok {
        bail!("'{s}' is not a {what} id: letters, digits, '.', '_' and '-'.");
    }
    Ok(s)
}

impl Head {
    pub fn source(&self) -> SourceId {
        SourceId {
            harness: self.harness.clone(),
            session: self.session.clone(),
            entry: self.entry.clone(),
        }
    }
}

impl Binding {
    pub fn is_attached(&self) -> bool {
        self.detached.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::Marker;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    fn round<T: Serialize + DeserializeOwned + PartialEq + fmt::Debug>(v: &T) {
        let line = serde_json::to_string(v).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(&serde_json::from_str::<T>(&line).unwrap(), v);
    }

    fn head() -> Head {
        Head {
            seq: 7,
            parent: Some(3),
            harness: "claude".into(),
            session: "s 1".into(),
            entry: "u1.1".into(),
            ts: "2026-10-14T10:00:00.123Z".parse().unwrap(),
        }
    }

    #[test]
    fn records_keep_text_and_identity_on_one_line() {
        round(&head());
        round(&Agent {
            harness: "claude".into(),
            agent: "a1".into(),
            agent_type: "general-purpose".into(),
            description: "Survey\nstorage\r\nexactly".into(),
            call: Some(2),
        });
        round(&Track {
            harness: "claude".into(),
            session: "s-1".into(),
            transcript: Some("/p with spaces/s-1.jsonl".into()),
            graft: None,
            tip: Some(4),
        });
        round(&Event {
            harness: "claude".into(),
            session: "s-1".into(),
            at: head().ts,
            state: State::Attached {
                thread: "dev".into(),
            },
        });
        let line = Line {
            ts: head().ts,
            kind: LineKind::Turn,
            who: Who::default(),
            text: "user: fix it\nAI: fixed\n".into(),
        };
        round(&LineOp::Put {
            key: Key { level: 1, last: 4 },
            line,
        });
        round(&LineOp::Drop {
            keys: vec![Key { level: 0, last: 2 }],
        });
        round(&LineOp::Clear);
    }

    #[test]
    fn rows_are_flat_with_tagged_kinds() {
        for kind in [
            Kind::User("hi\nthere".into()),
            Kind::Work {
                from: "research agent".into(),
                text: "found".into(),
            },
            Kind::Text(String::new()),
            Kind::ToolCall {
                id: "call-1".into(),
                name: "Bash".into(),
                input: json!({"command": "ls\n", "n": 18446744073709551615u64}),
            },
            Kind::ToolResult {
                id: "call-1".into(),
                output: "a\nb".into(),
                is_error: false,
            },
            Kind::Marker(Marker::Interrupt),
        ] {
            let line = serde_json::to_string(&Row {
                head: head(),
                kind: kind.clone(),
                ends_turn: false,
            })
            .unwrap();
            assert_eq!(
                serde_json::from_str::<Tag>(&line).unwrap().opens(),
                kind.opens()
            );
            round(&Row {
                head: head(),
                kind,
                ends_turn: true,
            });
        }
        let row = serde_json::to_value(Row {
            head: head(),
            kind: Kind::User("x".into()),
            ends_turn: false,
        })
        .unwrap();
        assert_eq!(row["seq"], 7);
        assert_eq!(row["kind"], "user");
        assert_eq!(row["content"], "x");
        assert!(row.get("ends_turn").is_none(), "written only when true");
        let mut unknown = row.clone();
        unknown["kind"] = "unknown".into();
        assert!(serde_json::from_value::<Row>(unknown).is_err());
    }

    #[test]
    fn missing_fields_fail_even_when_optional() {
        let fail = |v: serde_json::Value, field: &str| {
            let mut v = v;
            v.as_object_mut().unwrap().remove(field);
            v
        };
        let track = json!({"harness": "claude", "session": "s", "transcript": null, "graft": null, "tip": 4});
        assert!(serde_json::from_value::<Track>(track.clone()).is_ok());
        for f in ["transcript", "graft", "tip"] {
            assert!(
                serde_json::from_value::<Track>(fail(track.clone(), f)).is_err(),
                "{f}"
            );
        }
        let row = serde_json::to_value(Row {
            head: Head {
                parent: None,
                ..head()
            },
            kind: Kind::Text("t".into()),
            ends_turn: false,
        })
        .unwrap();
        assert!(serde_json::from_value::<Row>(fail(row, "parent")).is_err());
        let event = json!({"harness": "claude", "session": "s", "at": "2026-10-14T10:00:00Z", "state": "attached"});
        assert!(
            serde_json::from_value::<Event>(event).is_err(),
            "attached needs a thread"
        );
        let detached = json!({"harness": "claude", "session": "s", "state": "detached"});
        assert!(
            serde_json::from_value::<Event>(detached).is_err(),
            "an event needs its time"
        );
    }

    #[test]
    fn ids_are_path_names() {
        for ok in ["a87491ed-08a1", "claude", "s_1.2"] {
            assert!(check_id(ok, "session").is_ok());
        }
        for bad in ["", ".", "..", ".x", "a/b", "a b", "ü"] {
            assert!(check_id(bad, "session").is_err(), "{bad}");
        }
    }
}
