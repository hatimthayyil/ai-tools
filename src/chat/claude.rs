use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, BufRead},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde_json::Value;

use crate::chat::model::{Item, Kind, Marker, SourceId};

pub const HARNESS: &str = "claude";

const INTERRUPT: &str = "[Request interrupted by user";

/// The session running this process, from `CLAUDE_CODE_SESSION_ID`.
pub fn session_id() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let var = |k: &str| var(k).filter(|v| !v.is_empty());
    var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(var("HOME")?).join(".claude")))
}

/// The name of the project directory that holds the sessions started in `cwd`.
pub fn project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript of `session` under `config`: in the project directory of `cwd`, the
/// directory the session started in, else in whichever project directory holds it.
pub fn transcript(config: &Path, session: &str, cwd: &Path) -> Option<PathBuf> {
    if session.is_empty()
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let file = format!("{session}.jsonl");
    let projects = config.join("projects");
    let direct = projects.join(project_dir(cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

/// The directory the session of the transcript at `path` ran in: the `cwd` of its first entry
/// that records one.
pub fn session_dir(path: &Path) -> Option<PathBuf> {
    let f = fs::File::open(path).ok()?;
    io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .find_map(|l| {
            let v: Value = serde_json::from_str(&l).ok()?;
            v["cwd"]
                .as_str()
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
        })
}

/// The items of the transcript at `path`, as [`parse`] gives them.
pub fn read(path: &Path, session: &str) -> Result<Vec<Item>> {
    let bytes = fs::read(path).with_context(|| format!("Cannot read {}.", path.display()))?;
    Ok(parse_bytes(session, &bytes))
}

pub(super) fn parse_bytes(session: &str, bytes: &[u8]) -> Vec<Item> {
    let end = memchr::memrchr(b'\n', bytes).map_or(0, |i| i + 1);
    parse(session, &String::from_utf8_lossy(&bytes[..end]))
}

/// The items of a transcript, in file order, each linked to its logical parent. A last line
/// without its newline is still being written and is left for the next read.
pub fn parse(session: &str, text: &str) -> Vec<Item> {
    let mut chain = Chain::new(session);
    let end = text.rfind('\n').map_or(0, |i| i + 1);
    for line in text[..end].lines() {
        chain.push(line);
    }
    chain.items
}

/// A subagent's transcript, `<session>/subagents/agent-<agent>.jsonl`.
#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    pub agent: String,
    pub agent_type: String,
    pub description: String,
    /// The id of the Agent tool call that started it, in the parent's items.
    pub call: Option<String>,
    pub items: Vec<Item>,
}

/// The subagents of the session whose transcript is at `path`, by agent id.
pub fn subagents(path: &Path, session: &str) -> Result<Vec<Subagent>> {
    let files = super::source::capture(path)?;
    let name = Path::new(
        path.file_name()
            .context("A transcript needs a file name.")?,
    );
    Ok(subagents_from(&files, &name.with_extension(""), session))
}

pub(super) fn subagents_from(
    files: &BTreeMap<PathBuf, Vec<u8>>,
    companion: &Path,
    session: &str,
) -> Vec<Subagent> {
    let dir = companion.join("subagents");
    let mut out = Vec::new();
    for (file, bytes) in files {
        if file.parent() != Some(dir.as_path()) {
            continue;
        }
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let Some(agent) = name
            .strip_prefix("agent-")
            .and_then(|n| n.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let meta: Value = files
            .get(&file.with_extension("meta.json"))
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default();
        let field = |k: &str| meta[k].as_str().unwrap_or_default().to_string();
        out.push(Subagent {
            agent: agent.into(),
            agent_type: field("agentType"),
            description: field("description"),
            call: meta["toolUseId"].as_str().map(String::from),
            items: parse_bytes(session, bytes),
        });
    }
    out.sort_by(|a, b| a.agent.cmp(&b.agent));
    out
}

/// The transcript read so far: every chain entry by uuid, and the items made from them.
struct Chain<'s> {
    session: &'s str,
    entries: HashMap<String, Entry>,
    /// The last chain entry read: the parent of an entry whose parent was never written.
    previous: Option<String>,
    items: Vec<Item>,
    children: Vec<Vec<usize>>,
    calls: HashMap<String, usize>,
}

struct Entry {
    parent: Option<String>,
    /// The last item made from it; none for a dropped entry.
    item: Option<usize>,
}

impl<'s> Chain<'s> {
    fn new(session: &'s str) -> Chain<'s> {
        Chain {
            session,
            entries: HashMap::new(),
            previous: None,
            items: Vec::new(),
            children: Vec::new(),
            calls: HashMap::new(),
        }
    }

    fn push(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let Some(uuid) = v["uuid"].as_str() else {
            return;
        };
        if self.entries.contains_key(uuid) {
            return;
        }
        let parent = v["parentUuid"]
            .as_str()
            .or(v["logicalParentUuid"].as_str())
            .and_then(|p| match self.entries.contains_key(p) {
                true => Some(p.to_string()),
                false => self.previous.clone(),
            });
        let ts = v["timestamp"]
            .as_str()
            .and_then(|t| t.parse::<Timestamp>().ok());
        let (ts, kinds) = match ts {
            Some(ts) => (ts, kinds(&v)),
            None => (Timestamp::UNIX_EPOCH, Vec::new()),
        };
        let mut last = None;
        for (k, kind) in kinds.into_iter().enumerate() {
            let up = match (last, &kind) {
                (Some(prev), _) => Some(prev),
                (None, Kind::ToolResult { id, .. }) if self.calls.contains_key(id) => {
                    Some(self.tip(self.calls[id]))
                }
                _ => self.anchor(parent.as_deref()).map(|a| self.settle(a)),
            };
            let i = self.items.len();
            if let Kind::ToolCall { id, .. } = &kind {
                self.calls.insert(id.clone(), i);
            }
            let entry = match k {
                0 => uuid.to_string(),
                k => format!("{uuid}.{k}"),
            };
            self.items.push(Item {
                id: self.id(entry),
                parent: up.map(|p| self.items[p].id.clone()),
                ts,
                kind,
                ends_turn: false,
            });
            self.children.push(Vec::new());
            if let Some(p) = up {
                self.children[p].push(i);
            }
            last = Some(i);
        }
        if let Some(i) = last
            && v["type"] == "assistant"
            && v["message"]["stop_reason"] == "end_turn"
        {
            self.items[i].ends_turn = true;
        }
        self.entries
            .insert(uuid.to_string(), Entry { parent, item: last });
        self.previous = Some(uuid.to_string());
    }

    fn id(&self, entry: String) -> SourceId {
        SourceId {
            harness: HARNESS.into(),
            session: self.session.into(),
            entry,
        }
    }

    /// The nearest item at or above the entry `uuid`, past dropped entries.
    fn anchor<'a>(&'a self, mut uuid: Option<&'a str>) -> Option<usize> {
        for _ in 0..=self.entries.len() {
            let e = self.entries.get(uuid?)?;
            if e.item.is_some() {
                return e.item;
            }
            uuid = e.parent.as_deref();
        }
        None
    }

    /// The newest item under `i`: where a tool result lands, after its parallel calls and
    /// the results before it.
    fn tip(&self, mut i: usize) -> usize {
        while let Some(&c) = self.children[i].last() {
            i = c;
        }
        i
    }

    /// `i`, or the last tool result chained after it: a message hung on a call or an
    /// earlier result continues after the results.
    fn settle(&self, mut i: usize) -> usize {
        while let Some(&c) = self.children[i]
            .iter()
            .rev()
            .find(|&&c| matches!(self.items[c].kind, Kind::ToolResult { .. }))
        {
            i = c;
        }
        i
    }
}

/// What one entry says, in block order; nothing for a dropped entry.
fn kinds(v: &Value) -> Vec<Kind> {
    match v["type"].as_str() {
        Some("assistant") if v["message"]["model"] != "<synthetic>" => v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(assistant)
            .collect(),
        Some("user") => user(v),
        Some("attachment") => queued(&v["attachment"])
            .map(Kind::User)
            .into_iter()
            .collect(),
        Some("system") if v["subtype"] == "compact_boundary" => {
            vec![Kind::Marker(Marker::Compaction)]
        }
        _ => Vec::new(),
    }
}

fn assistant(block: &Value) -> Option<Kind> {
    match block["type"].as_str()? {
        "text" => block["text"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .map(|t| Kind::Text(t.into())),
        "tool_use" => Some(Kind::ToolCall {
            id: block["id"].as_str()?.into(),
            name: block["name"].as_str()?.into(),
            input: block["input"].clone(),
        }),
        _ => None,
    }
}

fn user(v: &Value) -> Vec<Kind> {
    if v["isCompactSummary"] == true {
        return vec![Kind::Marker(Marker::Summary)];
    }
    if let Some(work) = work(v) {
        return vec![work];
    }
    if v["isMeta"] == true {
        return Vec::new();
    }
    let content = &v["message"]["content"];
    let blocks = content.as_array().map_or(&[][..], Vec::as_slice);
    let mut out: Vec<Kind> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .filter_map(|b| {
            Some(Kind::ToolResult {
                id: b["tool_use_id"].as_str()?.into(),
                output: text(&b["content"]),
                is_error: b["is_error"] == true,
            })
        })
        .collect();
    let words = match content.as_str() {
        Some(s) => s.to_string(),
        None => text(&Value::Array(
            blocks
                .iter()
                .filter(|b| b["type"] != "tool_result")
                .cloned()
                .collect(),
        )),
    };
    if !out.is_empty() {
        if !words.trim().is_empty() {
            out.push(Kind::User(words));
        }
    } else if prompt(v, &words) {
        out.push(Kind::User(command(&words).unwrap_or(words)));
    } else if words.starts_with(INTERRUPT) {
        out.push(Kind::Marker(Marker::Interrupt));
    }
    out
}

/// A message from another agent: a peer's report or hand-back (its body, from its name), or
/// the coordinator's message to a subagent.
fn work(v: &Value) -> Option<Kind> {
    let origin = &v["origin"];
    let from = match origin["kind"].as_str()? {
        "peer" => origin["name"].as_str().or(origin["from"].as_str())?,
        "coordinator" => "coordinator",
        _ => return None,
    };
    let text = match origin["body"].as_str() {
        Some(body) => body.to_string(),
        None => text(&v["message"]["content"]),
    };
    Some(Kind::Work {
        from: from.into(),
        text,
    })
}

/// A user entry the user typed or sent, or a subagent's task; never injected context.
fn prompt(v: &Value, words: &str) -> bool {
    v["origin"]["kind"] == "human"
        || v["promptSource"] == "sdk"
        || words.starts_with("<command-message>")
        || (v["isSidechain"] == true && v["parentUuid"].is_null())
}

/// A message the user sent while the agent ran.
fn queued(a: &Value) -> Option<String> {
    let human = matches!(a["origin"]["kind"].as_str(), None | Some("human"));
    (a["type"] == "queued_command" && a["commandMode"] == "prompt" && human)
        .then(|| text(&a["prompt"]))
}

/// A prompt command as the user typed it: `/name args`.
fn command(s: &str) -> Option<String> {
    if !s.starts_with("<command-message>") {
        return None;
    }
    let tag = |t: &str| {
        let open = format!("<{t}>");
        let from = s.find(&open)? + open.len();
        Some(&s[from..from + s[from..].find(&format!("</{t}>"))?])
    };
    let name = tag("command-name")?;
    let args = tag("command-args").unwrap_or_default();
    Some(format!("{name} {args}").trim_end().to_string())
}

/// The text of a string or of content blocks; an image is `[image]`.
fn text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b["type"].as_str()? {
                "text" => Some(b["text"].as_str()?.to_string()),
                "image" => Some("[image]".into()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{
        model::{Tree, turns},
        record::Key,
        rollup,
    };

    const SESSION: &str = "s-1";

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude")
            .join(name)
    }

    fn load(name: &str) -> Vec<Item> {
        read(&fixture(name), SESSION).unwrap()
    }

    /// `entry<-parent kind` per item.
    fn shape(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| {
                let kind = match &i.kind {
                    Kind::User(_) => "user".to_string(),
                    Kind::Work { .. } => "work".into(),
                    Kind::Text(_) => "text".into(),
                    Kind::ToolCall { .. } => "call".into(),
                    Kind::ToolResult { .. } => "result".into(),
                    Kind::Marker(m) => format!("{m:?}").to_lowercase(),
                };
                let parent = i.parent.as_ref().map_or("", |p| p.entry.as_str());
                format!("{}<-{parent} {kind}", i.id.entry)
            })
            .collect()
    }

    /// Per turn on the newest branch: its first item, then the entries of each step.
    fn steps(items: Vec<Item>) -> Vec<Vec<Vec<String>>> {
        let tree = Tree::new(items);
        let path = tree.path(*tree.tips().last().unwrap());
        let entries = |r: std::ops::Range<usize>| -> Vec<String> {
            path[r].iter().map(|i| i.id.entry.clone()).collect()
        };
        turns(&path)
            .iter()
            .map(|x| {
                let first = x.span.start..x.span.start + 1;
                [entries(first)]
                    .into_iter()
                    .chain(x.steps.iter().map(|s| entries(s.clone())))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_plain_turn_keeps_only_words() {
        let items = load("plain.jsonl");
        assert_eq!(
            shape(&items),
            ["u1<- user", "a2<-u1 text", "u2<-a2 user", "a3<-u2 text"]
        );
        assert_eq!(items[2].kind, Kind::User("/deslop README.md".into()));
        let first = &items[0];
        assert_eq!(
            first.id,
            SourceId {
                harness: "claude".into(),
                session: SESSION.into(),
                entry: "u1".into()
            }
        );
        assert_eq!(
            first.ts,
            "2026-10-14T10:00:00Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn a_tool_run_is_cut_into_steps_at_each_text() {
        let items = load("tools.jsonl");
        assert_eq!(
            items[3].kind,
            Kind::ToolResult {
                id: "toolu_01read".into(),
                output: "1\tpub const PORT: u16 = 8000;".into(),
                is_error: false
            }
        );
        assert!(matches!(
            &items[10].kind,
            Kind::ToolResult { is_error: true, .. }
        ));
        assert_eq!(
            steps(items),
            [[
                vec!["u1"],
                vec!["a2", "a3", "r1", "a4", "r2"],
                vec!["a5", "a6", "r3", "a7", "r4", "a8", "r5"],
                vec!["a9"]
            ]]
        );
    }

    #[test]
    fn parallel_results_follow_their_calls_in_one_chain() {
        let want = [
            "u1<- user",
            "a1<-u1 text",
            "a2<-a1 call",
            "a3<-a2 call",
            "a4<-a3 call",
            "r1<-a4 result",
            "r2<-r1 result",
            "r3<-r2 result",
            "a5<-r3 text",
        ];
        assert_eq!(shape(&load("parallel.jsonl")), want);
        let text = fs::read_to_string(fixture("parallel.jsonl")).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(5, 6);
        let items = parse(SESSION, &(lines.join("\n") + "\n"));
        assert_eq!(Tree::new(items.clone()).tips(), [8], "results out of order");
        assert_eq!(
            shape(&items)[5..8],
            ["r2<-a4 result", "r1<-r2 result", "r3<-r1 result"]
        );
    }

    #[test]
    fn a_rewind_branches_and_keeps_the_old_branch() {
        let items = load("rewind.jsonl");
        assert_eq!(shape(&items)[6], "u3<-a1 user");
        let tree = Tree::new(items);
        let entry = |i: &Item| i.id.entry.clone();
        let ends: Vec<_> = tree
            .tips()
            .iter()
            .map(|&t| entry(&tree.items()[t]))
            .collect();
        assert_eq!(ends, ["a3", "a5"]);
        let path: Vec<_> = tree.path(9).into_iter().map(entry).collect();
        assert_eq!(path, ["u1", "a1", "u3", "a4", "r2", "a5"]);
    }

    #[test]
    fn compaction_is_a_marker_and_the_chain_goes_on() {
        let items = load("compact.jsonl");
        assert_eq!(
            shape(&items),
            [
                "u1<- user",
                "a1<-u1 text",
                "b1<-a1 compaction",
                "s1<-b1 summary",
                "u2<-s1 user",
                "a2<-u2 text"
            ]
        );
        assert_eq!(
            steps(items),
            [
                vec![vec!["u1"], vec!["a1", "b1", "s1"]],
                vec![vec!["u2"], vec!["a2"]]
            ]
        );
    }

    #[test]
    fn a_turn_ends_where_claude_ends_it_and_keeps_its_line_through_a_compaction() {
        let items = load("compact.jsonl");
        let ends: Vec<&str> = items
            .iter()
            .filter(|i| i.ends_turn)
            .map(|i| i.id.entry.as_str())
            .collect();
        assert_eq!(ends, ["a1", "a2"]);
        let lines = |n: usize| {
            let tree = Tree::new(items[..n].to_vec());
            let tip = *tree.tips().last().unwrap();
            rollup::branch(&tree, tip).lines()
        };
        let key = |last| Key { level: 1, last };
        assert_eq!(lines(1), [], "the turn is open");
        assert_eq!(lines(2), [key(1)], "the reply ends it");
        assert_eq!(lines(4), [key(1)], "the compaction after it moves no key");
        assert_eq!(lines(6), [key(1), key(5)]);
    }

    #[test]
    fn a_mid_run_message_opens_a_turn() {
        let items = load("interject.jsonl");
        assert_eq!(
            shape(&items),
            [
                "u1<- user",
                "a1<-u1 text",
                "a2<-a1 call",
                "r1<-a2 result",
                "q1<-r1 user",
                "a3<-q1 text",
                "a4<-a3 call",
                "r2<-a4 result",
                "a5<-r2 call",
                "r3<-a5 result",
                "i1<-r3 interrupt",
                "u2<-i1 user",
                "a6<-u2 call",
                "r4<-a6 result",
                "u3<-r4 user",
                "a7<-u3 text",
            ]
        );
        assert_eq!(items[4].kind, Kind::User("Skip the network tests.".into()));
        assert_eq!(
            steps(items),
            [
                vec![vec!["u1"], vec!["a1", "a2", "r1"]],
                vec![vec!["q1"], vec!["a3", "a4", "r2", "a5", "r3", "i1"]],
                vec![vec!["u2"], vec!["a6", "r4"]],
                vec![vec!["u3"], vec!["a7"]],
            ]
        );
        let items = load("interject-inline.jsonl");
        assert_eq!(
            shape(&items),
            [
                "u1<- user",
                "a1<-u1 call",
                "r1<-a1 result",
                "r1.1<-r1 user",
                "a2<-r1.1 text"
            ]
        );
        assert_eq!(
            items[3].kind,
            Kind::User("The licence is CC0-1.0, not MIT.".into())
        );
        let roll = rollup::Rollup::new(&Tree::new(items));
        let key = |level, last| Key { level, last };
        assert_eq!(
            roll.source(key(0, 2)),
            Some(&rollup::Source::Chunk {
                message: Some(0),
                items: vec![1, 2],
                whole: true,
            }),
            "the run the message cut short is work, with no final reply"
        );
        assert_eq!(
            roll.source(key(1, 4)),
            Some(&rollup::Source::Turn {
                message: Some(3),
                work: vec![],
                reply: Some(4),
            })
        );
        for name in ["interject.jsonl", "interject-inline.jsonl"] {
            let items = load(name);
            let roll = rollup::Rollup::new(&Tree::new(items.clone()));
            let opens = |seq: &u64| items[*seq as usize].kind.opens();
            for s in roll.lines().values() {
                match s {
                    rollup::Source::Chunk { items: work, .. } => {
                        assert!(!work.iter().any(opens), "{name}: no message is work")
                    }
                    rollup::Source::Turn { message, .. } => {
                        assert!(message.as_ref().is_some_and(opens), "{name}")
                    }
                    rollup::Source::Node(_) => {}
                }
            }
        }
    }

    #[test]
    fn a_growing_file_only_adds_items() {
        for name in [
            "plain.jsonl",
            "tools.jsonl",
            "parallel.jsonl",
            "rewind.jsonl",
            "compact.jsonl",
            "interject.jsonl",
            "interject-inline.jsonl",
            "subagent.jsonl",
        ] {
            let text = fs::read_to_string(fixture(name)).unwrap();
            let all = parse(SESSION, &text);
            let mut at = 0;
            for line in text.split_inclusive('\n') {
                let before = parse(SESSION, &text[..at]);
                let part = parse(SESSION, &text[..at + line.len() / 2]);
                assert_eq!(part, before, "{name}: a partial line");
                at += line.len();
                let whole = parse(SESSION, &text[..at]);
                assert_eq!(all[..whole.len()], whole[..], "{name}");
            }
        }
    }

    #[test]
    fn subagents_link_to_their_agent_call() {
        let path = fixture("subagent.jsonl");
        let items = read(&path, SESSION).unwrap();
        assert!(
            matches!(&items[1].kind, Kind::ToolCall { id, name, .. } if id == "toolu_50" && name == "Agent")
        );
        assert_eq!(
            shape(&items)[3..],
            ["a2<-r1 text", "w1<-a2 work", "a3<-w1 text"],
            "a hand-back opens a turn; a task notification is dropped"
        );
        assert_eq!(
            items[4].kind,
            Kind::Work {
                from: "researcher".into(),
                text: "Three of four tools keep one JSONL file per session; one uses SQLite."
                    .into()
            }
        );
        assert_eq!(
            steps(items),
            [
                vec![vec!["u1"], vec!["a1", "r1"], vec!["a2"]],
                vec![vec!["w1"], vec!["a3"]]
            ]
        );
        let subs = subagents(&path, SESSION).unwrap();
        assert_eq!(subs.len(), 1);
        let s = &subs[0];
        assert_eq!(
            (
                &*s.agent,
                &*s.agent_type,
                &*s.description,
                s.call.as_deref()
            ),
            (
                "a1b2c3d4e5f6a7b8c",
                "general-purpose",
                "Survey thread storage",
                Some("toolu_50")
            )
        );
        assert_eq!(
            shape(&s.items),
            [
                "s1<- user",
                "sa1<-s1 call",
                "sr1<-sa1 result",
                "sa2<-sr1 text",
                "c1<-sa2 work",
                "sa3<-c1 text"
            ]
        );
        assert_eq!(s.items[0].id.session, SESSION);
        assert_eq!(
            s.items[4].kind,
            Kind::Work {
                from: "coordinator".into(),
                text: "Also check how pi stores sessions.".into()
            }
        );
        assert!(
            subagents(&fixture("plain.jsonl"), SESSION)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn transcripts_are_found_by_session_and_start_dir() {
        assert_eq!(
            project_dir(Path::new("/home/user/.cache/my_app")),
            "-home-user--cache-my-app"
        );
        let config = tempfile::TempDir::new().unwrap();
        let here = config.path().join("projects/-work-app");
        let other = config.path().join("projects/-elsewhere");
        fs::create_dir_all(&here).unwrap();
        fs::create_dir_all(&other).unwrap();
        for f in [
            here.join("s-1.jsonl"),
            other.join("s-1.jsonl"),
            other.join("s-2.jsonl"),
        ] {
            fs::write(f, "").unwrap();
        }
        let find = |s: &str| transcript(config.path(), s, Path::new("/work/app"));
        assert_eq!(find("s-1"), Some(here.join("s-1.jsonl")));
        assert_eq!(
            find("s-2"),
            Some(other.join("s-2.jsonl")),
            "moved or hashed"
        );
        assert_eq!(find("s-3"), None);
        assert_eq!(find("../-elsewhere/s-2"), None);
        let env = |pairs: &'static [(&str, &str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
        };
        assert_eq!(
            config_dir(env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.claude"))
        );
        assert_eq!(
            config_dir(env(&[("HOME", "/h"), ("CLAUDE_CONFIG_DIR", "/c")])),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(config_dir(env(&[])), None);
    }
}
