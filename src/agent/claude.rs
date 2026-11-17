use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{self, BufRead},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use serde_json::Value;

use crate::agent::model::{Item, Kind, Marker, SourceId};

pub const HARNESS: &str = "claude";

const INTERRUPT: &str = "[Request interrupted by user";

/// The variable that names Claude Code's configuration directory.
const CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// The most characters Claude Code keeps of a session's name, from `--name` or `/rename`.
const NAME_MAX: usize = 200;

/// `address` as the name of a Claude Code session, the name other sessions send messages to;
/// an error if Claude Code would cut it.
pub fn session_name(address: &str) -> Result<&str> {
    let n = address.chars().count();
    if n > NAME_MAX {
        bail!(
            "Claude Code cuts a session's name at {NAME_MAX} characters, and {address} has {n}. \
             Give the agent, or the project's folder, a shorter name."
        );
    }
    Ok(address)
}

/// The session running this process, from `CLAUDE_CODE_SESSION_ID`.
pub fn session_id() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let var = |k: &str| var(k).filter(|v| !v.is_empty());
    var(CONFIG_DIR)
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(var("HOME")?).join(".claude")))
}

/// The configuration directory that keeps the session transcript `transcript`, which Claude
/// Code writes at `<directory>/projects/<project>/<session>.jsonl`; `None` for a path of
/// another shape.
pub fn config_of(transcript: &Path) -> Option<PathBuf> {
    let projects = transcript.parent()?.parent()?;
    (projects.file_name()? == "projects")
        .then(|| projects.parent().map(Path::to_path_buf))
        .flatten()
}

/// The sessions Claude Code runs on this machine, by session id, each with its name if it has
/// one: those `claude agents --json` lists, every live session and each background session
/// still working or blocked. Claude Code lists the sessions of one configuration directory, so
/// it is asked under ai's own environment, and under each of `configs` that exists, the
/// directories other sessions were started under.
pub fn running(
    configs: impl IntoIterator<Item = PathBuf>,
) -> Result<BTreeMap<String, Option<String>>> {
    let var = |k: &str| std::env::var(k).ok();
    let usual = config_dir(|k| var(k).filter(|_| k != CONFIG_DIR));
    let mut asked: BTreeSet<Option<PathBuf>> =
        [var(CONFIG_DIR).filter(|v| !v.is_empty()).map(PathBuf::from)].into();
    asked.extend(
        configs
            .into_iter()
            .filter(|c| c.is_dir())
            .map(|c| (Some(&c) != usual.as_ref()).then_some(c)),
    );
    let mut all = BTreeMap::new();
    for config in asked {
        all.extend(running_under(config.as_deref())?);
    }
    Ok(all)
}

/// The sessions `claude agents --json` lists under the configuration directory `config`, the
/// usual one when `None`.
fn running_under(config: Option<&Path>) -> Result<BTreeMap<String, Option<String>>> {
    let mut claude = Command::new("claude");
    claude.args(["agents", "--json"]).stdin(Stdio::null());
    match config {
        Some(dir) => claude.env(CONFIG_DIR, dir),
        None => claude.env_remove(CONFIG_DIR),
    };
    let o = claude
        .output()
        .map_err(|e| anyhow!("cannot run claude agents --json: {e}"))?;
    if !o.status.success() {
        bail!(
            "claude agents --json failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    parse_running(&o.stdout)
}

/// The sessions in `claude agents --json`'s output `bytes`, as [`running`] gives them.
fn parse_running(bytes: &[u8]) -> Result<BTreeMap<String, Option<String>>> {
    let sessions: Vec<Value> = serde_json::from_slice(bytes)
        .map_err(|e| anyhow!("claude agents --json printed what ai cannot read: {e}"))?;
    Ok(sessions
        .iter()
        .filter_map(|s| {
            let id = s["sessionId"].as_str()?;
            Some((id.to_string(), s["name"].as_str().map(String::from)))
        })
        .collect())
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
    let mut chain = Chain::default();
    chain.read(session, bytes);
    chain.items
}

/// `bytes` up to its last newline: a last line without one is still being written.
fn whole(bytes: &[u8]) -> &[u8] {
    &bytes[..memchr::memrchr(b'\n', bytes).map_or(0, |i| i + 1)]
}

/// The items of a transcript, in file order, each linked to its logical parent. A last line
/// without its newline is still being written and is left for the next read.
pub fn parse(session: &str, text: &str) -> Vec<Item> {
    parse_bytes(session, text.as_bytes())
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
    let dir = path
        .parent()
        .context("A transcript needs a parent directory.")?;
    subagents_from(dir, &files, &name.with_extension(""), session)
}

/// The subagents in `files`, the session's files by their path in `dir`, under `companion`: the
/// transcript's name without its extension. A subagent that ran on through a `/clear` wrote
/// its entries before it under another session; it is read whole, [`earlier`] parts first.
pub(super) fn subagents_from(
    dir: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
    companion: &Path,
    session: &str,
) -> Result<Vec<Subagent>> {
    let agents = companion.join("subagents");
    let mut out = Vec::new();
    for (file, bytes) in files {
        if file.parent() != Some(agents.as_path()) {
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
        let before = match first_parent(bytes) {
            Some(parent) => earlier(dir, companion, name, parent)?,
            None => Vec::new(),
        };
        let mut chain = Chain::default();
        for (session, bytes) in &before {
            chain.read(session, bytes);
        }
        chain.read(session, bytes);
        out.push(Subagent {
            agent: agent.into(),
            agent_type: field("agentType"),
            description: field("description"),
            call: meta["toolUseId"].as_str().map(String::from),
            items: chain.items,
        });
    }
    out.sort_by(|a, b| a.agent.cmp(&b.agent));
    Ok(out)
}

/// The entries of a transcript's whole lines; a line that is no JSON is skipped.
fn entries(bytes: &[u8]) -> impl Iterator<Item = Value> {
    whole(bytes)
        .split(|&b| b == b'\n')
        .filter_map(|l| serde_json::from_slice(l).ok())
}

/// The entry a transcript's first entry names as its parent: none where the transcript begins
/// its conversation, the last entry written before a `/clear` where a subagent ran on through
/// it.
fn first_parent(bytes: &[u8]) -> Option<String> {
    let first = entries(bytes).find(|v| v["uuid"].is_string())?;
    first["parentUuid"]
        .as_str()
        .or(first["logicalParentUuid"].as_str())
        .map(String::from)
}

/// What the subagent whose transcript is named `file` wrote before the entry that hangs on
/// `parent`, oldest first, each part with its session. A `/clear` moves a running subagent's
/// transcript to the new session's folder: each part sits under another session of `dir`,
/// beside `companion`, and the first entry of one names the last of the part before it.
fn earlier(
    dir: &Path,
    companion: &Path,
    file: &str,
    parent: String,
) -> Result<Vec<(String, Vec<u8>)>> {
    let cannot = |p: &Path| format!("Cannot read {}.", p.display());
    let mut parts = Vec::new();
    for e in fs::read_dir(dir).with_context(|| cannot(dir))? {
        let e = e.with_context(|| cannot(dir))?;
        let name = e.file_name();
        let Some(session) = name.to_str().filter(|s| Path::new(s) != companion) else {
            continue;
        };
        let part = e.path().join("subagents").join(file);
        match fs::read(&part) {
            Ok(bytes) => parts.push((session.to_string(), bytes)),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(e) => return Err(e).with_context(|| cannot(&part)),
        }
    }
    let mut out = Vec::new();
    let mut parent = Some(parent);
    while let Some(p) = parent {
        let holds = |bytes: &[u8]| {
            memchr::memmem::find(bytes, p.as_bytes()).is_some()
                && entries(bytes).any(|v| v["uuid"] == p.as_str())
        };
        let Some(at) = parts.iter().position(|(_, bytes)| holds(bytes)) else {
            break;
        };
        let part = parts.swap_remove(at);
        parent = first_parent(&part.1);
        out.push(part);
    }
    out.reverse();
    Ok(out)
}

/// The transcript read so far: every chain entry by uuid, and the items made from them.
#[derive(Default)]
struct Chain {
    /// The session that wrote the part being read.
    session: String,
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

impl Chain {
    /// Reads the whole lines of `bytes`, a part of the transcript that `session` wrote, after
    /// the parts read before it.
    fn read(&mut self, session: &str, bytes: &[u8]) {
        self.session = session.into();
        for line in String::from_utf8_lossy(whole(bytes)).lines() {
            self.push(line);
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
            session: self.session.clone(),
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
        Some("attachment") => queued(&v["attachment"]).into_iter().collect(),
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

/// A message from another agent: a peer's report or hand-back (its body, from its name), a
/// fork's report, or the coordinator's message to a subagent.
fn work(v: &Value) -> Option<Kind> {
    let origin = &v["origin"];
    let from = match origin["kind"].as_str()? {
        "peer" => origin["name"].as_str().or(origin["from"].as_str())?,
        "coordinator" => "coordinator",
        "task-notification" => return report(v["message"]["content"].as_str()?),
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
        || as_user(&v["origin"])
        || v["promptSource"] == "sdk"
        || words.starts_with("<command-message>")
        || (v["isSidechain"] == true && v["parentUuid"].is_null())
}

/// A plugin's prompt sent as the user's own words (`asUser`), as `/fold` sends its request for
/// a handoff and the user's direction; any other prompt of a plugin is context.
fn as_user(origin: &Value) -> bool {
    origin["kind"] == "plugin" && origin["asUser"] == true
}

/// A message that arrived while the agent ran: one the user sent, or a fork's report.
fn queued(a: &Value) -> Option<Kind> {
    if a["type"] != "queued_command" {
        return None;
    }
    let origin = &a["origin"];
    match a["commandMode"].as_str()? {
        "prompt" if matches!(origin["kind"].as_str(), None | Some("human")) || as_user(origin) => {
            Some(Kind::User(text(&a["prompt"])))
        }
        "task-notification" => report(a["prompt"].as_str()?),
        _ => None,
    }
}

/// A fork's report from a task notification, `Agent "<description>" <outcome>`: its result,
/// after the outcome unless it finished. Other tasks' notifications make none, and so does
/// one whose report came as a hand-back.
fn report(s: &str) -> Option<Kind> {
    let summary = unescape(tag(s, "summary")?);
    let rest = summary.strip_prefix("Agent \"")?;
    let end = ["\" finished", "\" failed", "\" was ", "\" is "]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()?;
    let result = tag(s, "result").map(unescape).unwrap_or_default();
    if result.contains("(its SubagentHandback call)") {
        return None;
    }
    let text = match &rest[end + 2..] {
        "finished" => result,
        outcome => format!("{outcome}\n\n{result}").trim_end().to_string(),
    };
    Some(Kind::Work {
        from: rest[..end].into(),
        text,
    })
}

/// A prompt command as the user typed it: `/name args`.
fn command(s: &str) -> Option<String> {
    if !s.starts_with("<command-message>") {
        return None;
    }
    let name = tag(s, "command-name")?;
    let args = tag(s, "command-args").unwrap_or_default();
    Some(format!("{name} {args}").trim_end().to_string())
}

/// The text of the first `<t>` element in `s`.
fn tag<'a>(s: &'a str, t: &str) -> Option<&'a str> {
    let open = format!("<{t}>");
    let from = s.find(&open)? + open.len();
    Some(&s[from..from + s[from..].find(&format!("</{t}>"))?])
}

/// Text a task notification escaped.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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
    use crate::agent::{
        model::{Tree, turns},
        record::Key,
        rollup,
    };

    #[test]
    fn running_sessions_read_from_claude_agents_json() {
        let json = br#"[
            {"cwd": "/r", "kind": "interactive", "startedAt": 1, "pid": 7, "status": "idle",
             "sessionId": "s-1", "name": "repo.dev"},
            {"cwd": "/r", "kind": "background", "startedAt": 2, "id": "bg1", "state": "working",
             "sessionId": "s-2"},
            {"cwd": "/r", "kind": "interactive", "startedAt": 3, "pid": 9, "status": "busy"}
        ]"#;
        assert_eq!(
            parse_running(json).unwrap(),
            BTreeMap::from([
                ("s-1".to_string(), Some("repo.dev".to_string())),
                ("s-2".to_string(), None),
            ])
        );
        assert!(parse_running(b"[]").unwrap().is_empty());
        let e = parse_running(b"").unwrap_err().to_string();
        assert!(
            e.starts_with("claude agents --json printed what ai cannot read: "),
            "{e}"
        );
    }

    const SESSION: &str = "s-1";

    #[test]
    fn a_session_name_is_its_address_up_to_the_characters_claude_code_keeps() {
        let fits = format!("p.{}", "a".repeat(NAME_MAX - 2));
        assert_eq!(session_name(&fits).unwrap(), fits);
        let wide = format!("p.{}", "é".repeat(NAME_MAX - 2));
        assert_eq!(
            session_name(&wide).unwrap(),
            wide,
            "characters count, not bytes"
        );
        let over = format!("{fits}a");
        let e = session_name(&over).unwrap_err().to_string();
        assert!(
            e.contains("at 200 characters") && e.contains("has 201"),
            "{e}"
        );
    }

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
            "report.jsonl",
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
            "a hand-back opens a turn; its notification is dropped"
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

    /// A project folder whose sessions each hold the given lines of the subagent fixture's
    /// transcript, as a `/clear` leaves a subagent that ran on through it.
    fn split(parts: &[(&str, std::ops::Range<usize>)]) -> tempfile::TempDir {
        const AGENT: &str = "agent-a1b2c3d4e5f6a7b8c";
        let dir = tempfile::TempDir::new().unwrap();
        let from = fixture("subagent/subagents");
        let text = fs::read_to_string(from.join(format!("{AGENT}.jsonl"))).unwrap();
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        for (session, range) in parts {
            let agents = dir.path().join(session).join("subagents");
            fs::create_dir_all(&agents).unwrap();
            let part = lines[range.clone()].concat();
            fs::write(agents.join(format!("{AGENT}.jsonl")), part).unwrap();
            let meta = format!("{AGENT}.meta.json");
            fs::copy(from.join(&meta), agents.join(&meta)).unwrap();
            fs::write(dir.path().join(format!("{session}.jsonl")), "").unwrap();
        }
        dir
    }

    #[test]
    fn a_subagent_that_ran_on_through_a_clear_reads_as_one() {
        let whole = subagents(&fixture("subagent.jsonl"), SESSION).unwrap();
        let sessions = |s: &Subagent| -> Vec<String> {
            s.items.iter().map(|i| i.id.session.clone()).collect()
        };

        let dir = split(&[("old", 0..4), ("new", 4..7)]);
        let subs = subagents(&dir.path().join("new.jsonl"), "new").unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(shape(&subs[0].items), shape(&whole[0].items));
        assert_eq!(subs[0].call.as_deref(), Some("toolu_50"));
        assert_eq!(
            sessions(&subs[0]),
            ["old", "old", "old", "new", "new", "new"]
        );
        let before = subagents(&dir.path().join("old.jsonl"), "old").unwrap();
        assert_eq!(
            before[0].items,
            subs[0].items[..3],
            "the session before reads its own part, the same items"
        );

        let dir = split(&[("a", 0..2), ("b", 2..4), ("c", 4..7)]);
        let subs = subagents(&dir.path().join("c.jsonl"), "c").unwrap();
        assert_eq!(shape(&subs[0].items), shape(&whole[0].items), "through two");
        assert_eq!(sessions(&subs[0]), ["a", "b", "b", "c", "c", "c"]);

        let dir = split(&[("new", 4..7)]);
        let subs = subagents(&dir.path().join("new.jsonl"), "new").unwrap();
        assert_eq!(
            shape(&subs[0].items),
            ["sa2<- text", "c1<-sa2 work", "sa3<-c1 text"],
            "without the part before, what is left starts the agent"
        );
    }

    #[test]
    fn a_row_a_plugin_appended_is_context_not_a_message() {
        let view =
            r#""message":{"role":"user","content":[{"type":"text","text":"You are awake."}]}"#;
        let at = r#""timestamp":"2026-11-11T08:00:00.000Z","sessionId":"s-1""#;
        let plugin = r#""type":"user","origin":{"kind":"plugin","name":"ai"}"#;
        let text = [
            format!(r#"{{"parentUuid":null,"uuid":"v1",{at},{plugin},"isMeta":true,{view}}}"#),
            format!(r#"{{"parentUuid":"v1","uuid":"v2",{at},{plugin},{view}}}"#),
            format!(
                r#"{{"parentUuid":"v2","uuid":"u1",{at},"type":"user","origin":{{"kind":"human"}},"message":{{"role":"user","content":"Go on."}}}}"#
            ),
        ]
        .map(|l| l + "\n")
        .concat();
        assert_eq!(shape(&parse(SESSION, &text)), ["u1<- user"]);
    }

    #[test]
    fn a_prompt_a_plugin_sent_as_the_users_words_is_their_message() {
        let at = r#""timestamp":"2026-11-17T08:00:00.000Z","sessionId":"s-1""#;
        let user = |uuid: &str, parent: &str, origin: &str, said: &str| {
            format!(
                r#"{{"parentUuid":{parent},"uuid":"{uuid}",{at},"type":"user","origin":{origin},"promptSource":"system","message":{{"role":"user","content":"{said}"}}}}"#
            )
        };
        let answer = |uuid: &str, parent: &str| {
            format!(
                r#"{{"parentUuid":"{parent}","uuid":"{uuid}",{at},"type":"assistant","message":{{"role":"assistant","model":"m","content":[{{"type":"text","text":"Done."}}]}}}}"#
            )
        };
        let queued = r#"{"parentUuid":"a2","uuid":"q1","timestamp":"2026-11-17T08:00:00.000Z","sessionId":"s-1","type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","prompt":"Queued.","origin":{"kind":"plugin","name":"ai","asUser":true}}}"#;
        let text = [
            user(
                "u1",
                "null",
                r#"{"kind":"plugin","name":"ai","asUser":true}"#,
                "Fold this session.",
            ),
            answer("a1", "u1"),
            user(
                "p1",
                r#""a1""#,
                r#"{"kind":"plugin","name":"ai"}"#,
                "The ai plugin sent a message: hi",
            ),
            answer("a2", "p1"),
            queued.to_string(),
        ]
        .map(|l| l + "\n")
        .concat();
        let items = parse(SESSION, &text);
        assert_eq!(
            shape(&items),
            ["u1<- user", "a1<-u1 text", "a2<-a1 text", "q1<-a2 user"],
            "a framed plugin prompt is context, so its answer goes on the turn before"
        );
        assert_eq!(items[0].kind, Kind::User("Fold this session.".into()));
        assert_eq!(items[3].kind, Kind::User("Queued.".into()));
    }

    #[test]
    fn a_fork_report_opens_a_turn_from_its_description() {
        let items = load("report.jsonl");
        assert_eq!(
            shape(&items),
            [
                "u1<- user",
                "a1<-u1 call",
                "r1<-a1 result",
                "a2<-r1 text",
                "t1<-a2 work",
                "a3<-t1 text",
                "u2<-a3 user",
                "a4<-u2 call",
                "q1<-a4 work",
                "r2<-q1 result",
                "a5<-r2 text",
            ],
            "a hand-back's notification and other tasks' make no item"
        );
        assert_eq!(
            items[4].kind,
            Kind::Work {
                from: "Fix the parser".into(),
                text: "Fixed `parse(<id>)` & added a test.".into()
            }
        );
        assert_eq!(
            items[8].kind,
            Kind::Work {
                from: "Write the docs".into(),
                text: "failed: Agent terminated early due to an API error: rate limit\n\n\
                       Halfway through README."
                    .into()
            }
        );
        assert_eq!(
            steps(items),
            [
                vec![vec!["u1"], vec!["a1", "r1"], vec!["a2"]],
                vec![vec!["t1"], vec!["a3"]],
                vec![vec!["u2"], vec!["a4"]],
                vec![vec!["q1"], vec!["r2"], vec!["a5"]],
            ]
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
