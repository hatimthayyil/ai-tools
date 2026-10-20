use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{RecvTimeoutError, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::{
    file::AtPath,
    memory::backend::{Backend, Block, Conversation, Usage},
};

const CALL_TIMEOUT: Duration = Duration::from_secs(300);
const KILL_GRACE: Duration = Duration::from_millis(200);
/// The directory of a store that keeps its compactor's calls, one [`Record`] each.
pub const CALLS: &str = "calls";

/// A random (version 4) UUID, as Claude Code's session ids.
pub fn uuid() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// The `claude` on `PATH`.
pub fn find() -> Result<OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path)
        .map(|d| d.join("claude"))
        .find(|p| p.is_file())
        .context("No claude on PATH: ai runs on Claude Code.")?;
    Ok(found.into())
}

/// The compactor's model: `$AI_MEMORY_MODEL`, default Sonnet.
pub fn model() -> String {
    std::env::var("AI_MEMORY_MODEL").unwrap_or_else(|_| "sonnet".into())
}

/// `claude -p` with the flags every call shares: stream-json both ways, isolated from the
/// user's settings, 5-minute cache marks. Callers add their own flags after `--tools`.
pub fn command(program: &OsStr, model: &str, effort: &str, system: &Path, tools: &str) -> Command {
    let mut c = Command::new(program);
    c.args(["-p", "--model", model, "--effort", effort])
        .args([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ])
        .args(["--verbose", "--include-partial-messages"])
        .args([
            "--no-session-persistence",
            "--setting-sources",
            "",
            "--strict-mcp-config",
        ])
        .arg("--system-prompt-file")
        .arg(system)
        .args(["--tools", tools])
        .env("CLAUDE_CODE_PROMPT_CACHE_TTL", "5m")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    die_with_parent(&mut c);
    c
}

/// The child dies even if it ignores SIGTERM when its spawning thread or process ends.
#[cfg(target_os = "linux")]
fn die_with_parent(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        c.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::other("the parent is gone"));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_parent(_: &mut Command) {}

fn terminate(pid: u32) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

fn kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Pids of running children, so an owner can end them all.
#[derive(Clone, Default)]
pub struct Live(Arc<Mutex<LiveState>>);

#[derive(Default)]
struct LiveState {
    pids: HashSet<u32>,
    stopped: bool,
}

impl Live {
    /// Ends every registered child and prevents future spawns through this registry.
    pub fn kill_all(&self) {
        let mut live = self.0.lock().expect("live lock");
        live.stopped = true;
        for &pid in &live.pids {
            terminate(pid);
            kill_group(pid);
        }
    }
}

/// One call's file `<session>.jsonl`, in order: an `ai.call` line with how `claude` was
/// started and its system prompt, an `ai.sent` line around each line written to its stdin,
/// and every line it printed, byte for byte.
#[derive(Clone)]
pub struct Record(Arc<Mutex<RecordFile>>);

struct RecordFile {
    path: PathBuf,
    file: File,
    error: Option<io::Error>,
}

impl Record {
    /// Creates the record of call `session` in `dir`, run as `cmd` under `system`.
    pub fn create(dir: &Path, session: &str, cmd: &Command, system: &str) -> Result<Record> {
        fs::create_dir_all(dir).at(dir)?;
        let path = dir.join(format!("{session}.jsonl"));
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)
            .at(&path)?;
        let os = |s: &OsStr| s.to_string_lossy().into_owned();
        let env: serde_json::Map<String, Value> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((os(k), Value::String(os(v?)))))
            .collect();
        let call = json!({
            "type": "ai.call",
            "program": os(cmd.get_program()),
            "args": cmd.get_args().map(os).collect::<Vec<_>>(),
            "env": env,
            "system": system,
        });
        let r = Record(Arc::new(Mutex::new(RecordFile {
            path,
            file,
            error: None,
        })));
        r.write(format!("{call}\n").as_bytes());
        r.check()?;
        Ok(r)
    }

    /// Appends `bytes` as they are, unless a write already failed.
    fn write(&self, bytes: &[u8]) {
        let mut f = self.0.lock().expect("record lock");
        if f.error.is_none()
            && let Err(e) = f.file.write_all(bytes)
        {
            f.error = Some(e);
        }
    }

    /// Appends `line`, written to the call's stdin, as `{"type":"ai.sent","line":<line>}`.
    fn sent(&self, line: &str) {
        self.write(format!("{{\"type\":\"ai.sent\",\"line\":{line}}}\n").as_bytes());
    }

    /// An error once a write failed: the record is no longer whole.
    fn check(&self) -> Result<()> {
        let f = self.0.lock().expect("record lock");
        match &f.error {
            Some(e) => bail!("Cannot write {}: {e}", f.path.display()),
            None => Ok(()),
        }
    }
}

/// A running `claude`: stream-json messages in, stdout lines out through a sink.
pub struct Proc {
    child: Option<Child>,
    stdin: Option<Sender<String>>,
    stderr: Arc<Mutex<String>>,
    live: Option<Live>,
    record: Option<Record>,
}

impl Proc {
    /// Starts `cmd`; a reader thread hands `sink` each stdout line, then `None` at its end.
    /// With `record`, what goes in and every byte that comes out is kept there.
    pub fn spawn(
        mut cmd: Command,
        live: Option<&Live>,
        record: Option<Record>,
        mut sink: impl FnMut(Option<String>) + Send + 'static,
    ) -> Result<Proc> {
        let mut registry = live.map(|l| l.0.lock().expect("live lock"));
        if registry.as_ref().is_some_and(|l| l.stopped) {
            bail!("claude owner has stopped");
        }
        let child = cmd
            .spawn()
            .context("Cannot run claude. Is Claude Code installed and on PATH?")?;
        if let Some(l) = &mut registry {
            l.pids.insert(child.id());
        }
        drop(registry);
        let mut proc = Proc {
            child: Some(child),
            stdin: None,
            stderr: Arc::new(Mutex::new(String::new())),
            live: live.cloned(),
            record: record.clone(),
        };
        let child = proc.child.as_mut().expect("spawned child");
        let stdout = child.stdout.take().context("No stdout.")?;
        let mut err = child.stderr.take().context("No stderr.")?;
        proc.stdin = child.stdin.take().map(|mut pipe| {
            let (tx, rx) = channel::<String>();
            thread::spawn(move || {
                for message in rx {
                    if writeln!(pipe, "{message}")
                        .and_then(|()| pipe.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            });
            tx
        });
        thread::spawn(move || {
            let mut out = BufReader::new(stdout);
            let mut raw = Vec::new();
            while out.read_until(b'\n', &mut raw).is_ok_and(|n| n > 0) {
                if let Some(r) = &record {
                    r.write(&raw);
                }
                let line = raw.strip_suffix(b"\n").unwrap_or(&raw);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                sink(Some(String::from_utf8_lossy(line).into_owned()));
                raw.clear();
            }
            sink(None);
        });
        let buf = proc.stderr.clone();
        thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(n @ 1..) = err.read(&mut chunk) {
                buf.lock()
                    .expect("stderr lock")
                    .push_str(&String::from_utf8_lossy(&chunk[..n]));
            }
        });
        Ok(proc)
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// Queues one stream-json user message without blocking on the child reading stdin.
    pub fn send(&mut self, blocks: &[Block]) -> io::Result<()> {
        let stdin = self.stdin.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        let line = user_message(blocks);
        if let Some(r) = &self.record {
            r.sent(&line);
        }
        stdin
            .send(line)
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }

    /// Sends SIGTERM to the child group; dropping it bounds the wait and reaps it.
    pub fn kill(&self) {
        if let Some(c) = &self.child {
            terminate(c.id());
        }
    }

    /// Why the child ended without a result: its exit code and the tail of its stderr.
    pub fn failure(&mut self) -> String {
        if let Some(child) = &self.child {
            kill_group(child.id());
        }
        let code = self.child.as_mut().and_then(|c| c.wait().ok()?.code());
        thread::sleep(Duration::from_millis(50));
        let err = self.stderr.lock().expect("stderr lock").clone();
        let err = match err.trim() {
            "" => "no error output",
            e => tail(e, 300),
        };
        format!("claude exited (code {code:?}) before its result: {err}")
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        self.stdin.take();
        terminate(child.id());
        let deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < deadline && !matches!(child.try_wait(), Ok(Some(_))) {
            thread::sleep(Duration::from_millis(10));
        }
        kill_group(child.id());
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(l) = &self.live {
            l.0.lock().expect("live lock").pids.remove(&child.id());
        }
    }
}

/// Writes each distinct system prompt once into a temporary directory.
pub struct Prompts(TempDir);

impl Prompts {
    pub fn new() -> Result<Prompts> {
        Ok(Prompts(
            TempDir::new().context("Cannot create a temporary directory.")?,
        ))
    }

    pub fn file(&self, system: &str) -> Result<PathBuf> {
        let mut h = DefaultHasher::new();
        system.hash(&mut h);
        let p = self.0.path().join(format!("{:016x}.txt", h.finish()));
        if !p.exists() {
            fs::write(&p, system).at(&p)?;
        }
        Ok(p)
    }
}

/// Claude Code in print mode (`claude -p`), isolated from the user's settings, without tools.
/// Each call keeps its [`Record`] in `calls`.
pub struct ClaudeCode {
    program: OsString,
    model: String,
    effort: String,
    prompts: Prompts,
    live: Live,
    calls: PathBuf,
}

impl ClaudeCode {
    /// The compactor: `$AI_MEMORY_MODEL` (default Sonnet) at medium effort, through the
    /// `claude` on `PATH`, keeping its calls in `calls`.
    pub fn compactor(calls: &Path) -> Result<ClaudeCode> {
        ClaudeCode::compactor_at(find()?, &model(), calls)
    }

    pub fn compactor_at(program: OsString, model: &str, calls: &Path) -> Result<ClaudeCode> {
        Ok(ClaudeCode {
            program,
            model: model.into(),
            effort: "medium".into(),
            prompts: Prompts::new()?,
            live: Live::default(),
            calls: calls.into(),
        })
    }

    pub fn command(&self, system_file: &Path, session: &str) -> Command {
        let mut c = command(&self.program, &self.model, &self.effort, system_file, "");
        c.args(["--safe-mode", "--session-id", session])
            .env("DISABLE_PROMPT_CACHING", "1");
        c
    }
}

impl Backend for ClaudeCode {
    fn agent(&self) -> &str {
        "claude-code"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn open(&self, system: &str) -> Result<Box<dyn Conversation>> {
        let id = uuid()?;
        let cmd = self.command(&self.prompts.file(system)?, &id);
        let record = Record::create(&self.calls, &id, &cmd, system)?;
        let (tx, lines) = channel();
        let proc = Proc::spawn(cmd, Some(&self.live), Some(record.clone()), move |l| {
            if let Some(l) = l {
                let _ = tx.send(l);
            }
        })?;
        Ok(Box::new(Session {
            proc,
            lines,
            id,
            record,
            used: Usage::default(),
            cost: 0.0,
        }))
    }

    fn stop(&self) {
        self.live.kill_all();
    }
}

struct Session {
    proc: Proc,
    lines: std::sync::mpsc::Receiver<String>,
    id: String,
    record: Record,
    /// What the last reply took.
    used: Usage,
    /// The process's `total_cost_usd` so far.
    cost: f64,
}

/// What the request a `result` event ends took, `before` being the `total_cost_usd` of the
/// process's earlier results: its `usage`, and the growth of `total_cost_usd`, which counts
/// the whole process.
pub fn usage(ev: &Value, before: f64) -> Usage {
    let n = |k: &str| ev["usage"][k].as_u64().unwrap_or(0);
    Usage {
        input: n("input_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
        output: n("output_tokens"),
        cost: ev["total_cost_usd"].as_f64().map_or(0.0, |t| t - before),
    }
}

/// A stream-json user message.
pub fn user_message(message: &[Block]) -> String {
    let content: Vec<Value> = message
        .iter()
        .map(|b| match b.cache {
            true => json!({"type": "text", "text": b.text, "cache_control": {"type": "ephemeral"}}),
            false => json!({"type": "text", "text": b.text}),
        })
        .collect();
    json!({"type": "user", "message": {"role": "user", "content": content}}).to_string()
}

/// The reply a `result` event carries.
pub fn reply(ev: &Value) -> Result<String> {
    let text = ev["result"].as_str().unwrap_or_default().to_string();
    if ev["is_error"].as_bool().unwrap_or(false) {
        let why = if text.is_empty() {
            ev["subtype"].as_str().unwrap_or("error").to_string()
        } else {
            text
        };
        bail!("claude: {}", tail(&why, 300))
    } else if ev["stop_reason"] == "refusal" {
        bail!("claude refused")
    }
    Ok(text)
}

pub fn tail(s: &str, n: usize) -> &str {
    let s = s.trim();
    &s[s.ceil_char_boundary(s.len().saturating_sub(n))..]
}

impl Conversation for Session {
    fn say(&mut self, message: &[Block]) -> Result<String> {
        self.used = Usage::default();
        let _ = self.proc.send(message);
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(wait) {
                Ok(line) => {
                    let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if let Some(id) = ev["session_id"].as_str() {
                        self.id = id.into();
                    }
                    if ev["type"] == "result" {
                        self.used = usage(&ev, self.cost);
                        self.cost += self.used.cost;
                        self.record.check()?;
                        return reply(&ev);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    bail!("claude: no result after {} s.", CALL_TIMEOUT.as_secs())
                }
                Err(RecvTimeoutError::Disconnected) => bail!("{}", self.proc.failure()),
            }
        }
    }

    fn session(&self) -> String {
        self.id.clone()
    }

    fn used(&self) -> Usage {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_version_4() {
        let u = uuid().unwrap();
        assert_eq!(u.len(), 36);
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!("89ab".contains(&parts[3][..1]));
        assert_ne!(uuid().unwrap(), u);
    }

    #[test]
    fn a_call_keeps_what_went_in_and_every_byte_out() {
        let tmp = TempDir::new().unwrap();
        let program = tmp.path().join("claude");
        fs::write(
            &program,
            r#"#!/bin/sh
while [ "$1" != --session-id ]; do shift; done
n=0
while read -r line; do
  n=$((n+1))
  printf '{"type":"system","session_id":"%s"}\n' "$2"
  printf 'not json \377\r\n'
  printf '{"type":"result","is_error":false,"result":"r%s","usage":{"input_tokens":10,"cache_creation_input_tokens":1,"cache_read_input_tokens":2,"output_tokens":3},"total_cost_usd":0.%s5,"session_id":"%s"}\n' $n $n "$2"
done
"#,
        )
        .unwrap();
        fs::set_permissions(
            &program,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let calls = tmp.path().join(CALLS);
        let c = ClaudeCode::compactor_at(program.into(), "sonnet", &calls).unwrap();
        let mut conv = c.open("Write a line.").unwrap();
        assert_eq!(conv.say(&[Block::new("a", false)]).unwrap(), "r1");
        let first = conv.used();
        assert_eq!(
            (
                first.input,
                first.cache_write,
                first.cache_read,
                first.output
            ),
            (10, 1, 2, 3)
        );
        assert!((first.cost - 0.15).abs() < 1e-9, "{first:?}");
        assert_eq!(conv.say(&[Block::new("b", false)]).unwrap(), "r2");
        assert!((conv.used().cost - 0.1).abs() < 1e-9, "{:?}", conv.used());
        let id = conv.session();
        drop(conv);
        let got = fs::read(calls.join(format!("{id}.jsonl"))).unwrap();
        let mut lines = got.split_inclusive(|&b| b == b'\n');
        let call: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
        assert_eq!(call["type"], "ai.call");
        assert_eq!(call["system"], "Write a line.");
        assert_eq!(call["env"]["DISABLE_PROMPT_CACHING"], "1");
        assert!(call["args"].as_array().unwrap().contains(&json!(id)));
        let rest: Vec<&[u8]> = lines.collect();
        let result = |n: u32| {
            format!(
                r#"{{"type":"result","is_error":false,"result":"r{n}","usage":{{"input_tokens":10,"cache_creation_input_tokens":1,"cache_read_input_tokens":2,"output_tokens":3}},"total_cost_usd":0.{n}5,"session_id":"{id}"}}"#
            )
        };
        let sent = |t: &str| {
            format!(
                "{{\"type\":\"ai.sent\",\"line\":{}}}\n",
                user_message(&[Block::new(t, false)])
            )
        };
        let system = format!("{{\"type\":\"system\",\"session_id\":\"{id}\"}}\n");
        let junk: &[u8] = b"not json \xff\r\n";
        let want: Vec<Vec<u8>> = vec![
            sent("a").into(),
            system.clone().into(),
            junk.into(),
            format!("{}\n", result(1)).into(),
            sent("b").into(),
            system.into(),
            junk.into(),
            format!("{}\n", result(2)).into(),
        ];
        assert_eq!(rest, want, "{}", String::from_utf8_lossy(&got));
    }

    #[test]
    fn the_compactor_runs_isolated_without_tools() {
        let tmp = TempDir::new().unwrap();
        let c = ClaudeCode::compactor_at("claude".into(), "sonnet", tmp.path()).unwrap();
        let file = c.prompts.file("You write the memory.").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "You write the memory.");
        assert_eq!(c.prompts.file("You write the memory.").unwrap(), file);
        let cmd = c.command(&file, "s1");
        assert_eq!(cmd.get_program(), "claude");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        let f = file.to_str().unwrap();
        assert_eq!(
            args,
            [
                "-p",
                "--model",
                "sonnet",
                "--effort",
                "medium",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--no-session-persistence",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--system-prompt-file",
                f,
                "--tools",
                "",
                "--safe-mode",
                "--session-id",
                "s1",
            ]
        );
        let mut env: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.and_then(|v| v.to_str())))
            .collect();
        env.sort();
        assert_eq!(
            env,
            [
                ("CLAUDE_CODE_PROMPT_CACHE_TTL", Some("5m")),
                ("DISABLE_PROMPT_CACHING", Some("1"))
            ]
        );
        assert_eq!(c.model(), "sonnet");
    }

    #[test]
    fn stream_json_in_and_out() {
        let m = user_message(&[Block::new("a", true), Block::new("b", false)]);
        let v: Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(
            v["message"]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(v["message"]["content"][1].get("cache_control").is_none());
        let result = |line: &str| reply(&serde_json::from_str(line).unwrap());
        let ok = result(
            r#"{"type":"result","is_error":false,"result":" a line ","stop_reason":"end_turn"}"#,
        );
        assert_eq!(ok.unwrap(), " a line ");
        let err = result(r#"{"type":"result","is_error":true,"subtype":"error_max_turns"}"#);
        assert!(err.unwrap_err().to_string().contains("error_max_turns"));
        let refused =
            result(r#"{"type":"result","is_error":false,"result":"","stop_reason":"refusal"}"#);
        assert!(refused.is_err());
    }

    #[test]
    fn children_end_with_their_owner() {
        let live = Live::default();
        let mut c = Command::new("sleep");
        c.arg("60").stdin(Stdio::piped()).stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        die_with_parent(&mut c);
        let (tx, rx) = channel();
        let p = Proc::spawn(c, Some(&live), None, move |l| {
            let _ = tx.send(l);
        })
        .unwrap();
        live.kill_all();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), None);
        drop(p);
        assert!(live.0.lock().unwrap().pids.is_empty());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod cleanup_tests {
    use super::*;
    fn shell(code: &str) -> Command {
        let mut c = Command::new("bash");
        c.args(["-c", code])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        die_with_parent(&mut c);
        c
    }
    fn running(pid: u32) -> bool {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|s| s.split_whitespace().nth(2).is_none_or(|state| state != "Z"))
    }
    #[test]
    fn eof_from_a_live_ignoring_term_child_is_bounded_and_reaped() {
        let (tx, rx) = channel();
        let mut proc = Proc::spawn(
            shell("trap '' TERM; exec 1>&-; sleep 60"),
            None,
            None,
            move |l| {
                let _ = tx.send(l);
            },
        )
        .unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), None);
        let pid = proc.pid();
        let at = Instant::now();
        let why = proc.failure();
        drop(proc);
        assert!(why.contains("before its result"));
        assert!(at.elapsed() < Duration::from_secs(1));
        assert!(!running(pid));
    }
    #[test]
    fn dropping_owner_ends_ignoring_term_descendants_and_prevents_new_calls() {
        let live = Live::default();
        let (tx, rx) = channel();
        let proc = Proc::spawn(
            shell("trap '' TERM; sleep 60 & echo $!; wait"),
            Some(&live),
            None,
            move |l| {
                let _ = tx.send(l);
            },
        )
        .unwrap();
        let child: u32 = rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(running(child));
        let at = Instant::now();
        drop(proc);
        assert!(at.elapsed() < Duration::from_secs(1));
        let deadline = Instant::now() + Duration::from_secs(1);
        while running(child) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!running(child));
        live.kill_all();
        assert!(Proc::spawn(shell("sleep 60"), Some(&live), None, |_| {}).is_err());
    }
    #[test]
    fn parent_death_kills_an_ignoring_term_claude() {
        let mut owner = Command::new("bash");
        // A separate copy of this test executable owns the PDEATHSIG-protected child.
        let temp = TempDir::new().unwrap();
        let pidfile = temp.path().join("pid");
        owner.arg("-c").arg("exec \"$1\" --ignored --exact memory::claude::cleanup_tests::parent_death_helper --nocapture").arg("owner").arg(std::env::current_exe().unwrap()).env("AI_CHILD_PIDFILE", &pidfile).stdout(Stdio::null()).stderr(Stdio::null());
        let mut owner = owner.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pidfile.exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let pid: u32 = fs::read_to_string(&pidfile).unwrap().parse().unwrap();
        owner.kill().unwrap();
        owner.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while running(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!running(pid));
    }
    #[test]
    #[ignore]
    fn parent_death_helper() {
        let p = Proc::spawn(
            shell("trap '' TERM; while :; do :; done"),
            None,
            None,
            |_| {},
        )
        .unwrap();
        fs::write(
            std::env::var_os("AI_CHILD_PIDFILE").unwrap(),
            p.pid().to_string(),
        )
        .unwrap();
        thread::sleep(Duration::from_secs(60));
    }
}
