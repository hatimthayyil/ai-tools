use std::{fmt, path::PathBuf};

use anyhow::{Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    chat::model::{Item, Kind, SourceId},
    memory::Who,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub seq: u64,
    pub parent: Option<u64>,
    pub harness: String,
    pub session: String,
    pub entry: String,
    pub ts: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub call: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

impl LineKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Chunk => "chunk",
            Self::Turn => "exchange",
            Self::Node => "node",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub ts: Timestamp,
    pub kind: LineKind,
    #[serde(with = "WhoRecord")]
    pub who: Who,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Who")]
struct WhoRecord {
    agent: String,
    model: String,
    session: String,
}

pub(super) fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

pub(super) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(bytes)?)
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

pub(super) fn item(head: &Head, parent: Option<SourceId>, blob: &[u8], raw: &[u8]) -> Result<Item> {
    Ok(Item {
        id: head.source(),
        parent,
        ts: head.ts,
        kind: decode::<Kind>(blob)?,
        raw: String::from_utf8(raw.to_vec())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::Marker;
    use serde_json::json;

    #[test]
    fn structured_records_preserve_text_and_identity() {
        let head = Head {
            seq: 7,
            parent: Some(3),
            harness: "claude".into(),
            session: "s 1".into(),
            entry: "u1.1".into(),
            ts: "2026-10-14T10:00:00.123Z".parse().unwrap(),
        };
        assert_eq!(decode::<Head>(&encode(&head).unwrap()).unwrap(), head);
        let agent = Agent {
            harness: "claude".into(),
            agent: "a1".into(),
            agent_type: "general-purpose".into(),
            description: "Survey\nstorage\r\nexactly".into(),
            call: Some(2),
        };
        assert_eq!(decode::<Agent>(&encode(&agent).unwrap()).unwrap(), agent);
        let binding = Binding {
            harness: "claude".into(),
            session: "s-1".into(),
            transcript: Some("/p with spaces/s-1.jsonl".into()),
            graft: None,
            tip: Some(4),
            attached: head.ts,
            detached: None,
        };
        assert_eq!(
            decode::<Binding>(&encode(&binding).unwrap()).unwrap(),
            binding
        );
        let line = Line {
            ts: head.ts,
            kind: LineKind::Turn,
            who: Who::default(),
            text: "user: fix it\nAI: fixed\n".into(),
        };
        assert_eq!(decode::<Line>(&encode(&line).unwrap()).unwrap(), line);
        assert!(decode::<Binding>(br#"{"tip":4}"#).is_err());
    }

    #[test]
    fn item_kinds_use_tagged_json() {
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
                input: json!({"command": "ls\n"}),
            },
            Kind::ToolResult {
                id: "call-1".into(),
                output: "a\nb".into(),
                is_error: false,
            },
            Kind::Marker(Marker::Interrupt),
        ] {
            assert_eq!(decode::<Kind>(&encode(&kind).unwrap()).unwrap(), kind);
        }
        assert!(decode::<Kind>(br#"{"kind":"unknown","content":"x"}"#).is_err());
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
