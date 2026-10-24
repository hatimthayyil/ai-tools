use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use gix::bstr::ByteSlice;

use crate::{
    agent::claude,
    memory::record::{Place, Who},
};

const CODEX: [&str; 3] = [
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
];

/// The git repository at `cwd`: its identity, `HEAD` and branch.
pub fn place(cwd: &Path) -> Place {
    let Ok(repo) = gix::discover(cwd) else {
        return Place::default();
    };
    Place {
        repo: identity(&repo).unwrap_or_default(),
        project: String::new(),
        head: repo
            .head_id()
            .map(|id| id.to_hex_with_len(12).to_string())
            .unwrap_or_default(),
        branch: repo
            .head_name()
            .ok()
            .flatten()
            .map(|n| n.shorten().to_string())
            .unwrap_or_default(),
    }
}

fn identity(repo: &gix::Repository) -> Option<String> {
    let url = repo.config_snapshot().string("remote.origin.url");
    url.and_then(|u| owner_name(u.as_bstr())).or_else(|| {
        let root = repo.workdir().unwrap_or(repo.git_dir());
        Some(root.file_name()?.to_string_lossy().into_owned())
    })
}

fn owner_name(url: &gix::bstr::BStr) -> Option<String> {
    let url = gix::url::parse(url).ok()?;
    let path = url.path.to_str_lossy();
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit(['/', ':']).filter(|p| !p.is_empty());
    let name = parts.next()?;
    Some(format!("{}/{name}", parts.next()?))
}

/// The agent, model and session, read through `var`.
pub fn who(var: impl Fn(&str) -> Option<String>) -> Who {
    let var = |k: &str| var(k).filter(|v| !v.is_empty());
    let agent = var("AI_AGENT").or_else(|| {
        if var("CLAUDECODE").is_some() {
            Some("claude-code".into())
        } else if CODEX.iter().any(|k| var(k).is_some()) {
            Some("codex".into())
        } else {
            None
        }
    });
    let model = var("AI_MODEL").or_else(|| {
        if let Some(id) = var("CLAUDE_CODE_SESSION_ID") {
            claude_model(&claude::config_dir(var)?, &id)
        } else if let Some(id) = var("CODEX_THREAD_ID") {
            let dir = var("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| Some(PathBuf::from(var("HOME")?).join(".codex")))?;
            codex_model(&dir, &id)
        } else {
            None
        }
    });
    Who {
        agent: agent.unwrap_or_default(),
        model: model.unwrap_or_default(),
        session: var("CLAUDE_CODE_SESSION_ID")
            .or_else(|| var("CODEX_THREAD_ID"))
            .or_else(|| var("AI_SESSION"))
            .unwrap_or_default(),
    }
}

fn safe_id(id: &str) -> bool {
    id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn claude_model(dir: &Path, id: &str) -> Option<String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let path = claude::transcript(dir, id, &cwd)?;
    last_model(&path, r#""type":"assistant""#, "/message/model")
}

fn codex_model(dir: &Path, id: &str) -> Option<String> {
    if !safe_id(id) {
        return None;
    }
    let suffix = format!("-{id}.jsonl");
    let path = newest_first(&dir.join("sessions"), 3)
        .into_iter()
        .find(|p| p.to_str().is_some_and(|s| s.ends_with(&suffix)))?;
    last_model(&path, r#""type":"turn_context""#, "/payload/model")
}

fn newest_first(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort_unstable_by(|a, b| b.cmp(a));
    if depth == 0 {
        return paths;
    }
    paths
        .iter()
        .filter(|p| p.is_dir())
        .flat_map(|p| newest_first(p, depth - 1))
        .collect()
}

const TAIL: u64 = 64 << 10;

fn last_model(path: &Path, needle: &str, pointer: &str) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut size = TAIL;
    loop {
        let start = len.saturating_sub(size);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::new();
        (&mut f).take(len - start).read_to_end(&mut buf).ok()?;
        let floor = if start == 0 {
            0
        } else {
            memchr::memchr(b'\n', &buf).map_or(buf.len(), |i| i + 1)
        };
        let mut end = buf.len();
        while let Some(i) = memchr::memmem::rfind(&buf[floor..end], needle.as_bytes()) {
            let i = floor + i;
            let from = memchr::memrchr(b'\n', &buf[..i]).map_or(0, |p| p + 1);
            let to = memchr::memchr(b'\n', &buf[i..]).map_or(buf.len(), |p| i + p);
            let model = serde_json::from_slice::<serde_json::Value>(&buf[from..to])
                .ok()
                .and_then(|v| v.pointer(pointer)?.as_str().map(str::to_owned))
                .filter(|m| !m.is_empty() && !m.starts_with('<'));
            if model.is_some() {
                return model;
            }
            end = from;
        }
        if start == 0 {
            return None;
        }
        size *= 16;
    }
}

pub fn env_who() -> Who {
    who(|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(&str, &str)]) -> Who {
        who(|k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn agents() {
        assert_eq!(with(&[]), Who::default());
        assert_eq!(with(&[("CLAUDECODE", "1")]).agent, "claude-code");
        assert_eq!(with(&[("CODEX_SANDBOX", "seatbelt")]).agent, "codex");
        let w = with(&[
            ("CLAUDECODE", "1"),
            ("AI_AGENT", "pi"),
            ("AI_MODEL", "opus"),
            ("AI_SESSION", "s2"),
            ("CLAUDE_CODE_SESSION_ID", "s1"),
        ]);
        assert_eq!(
            (w.agent.as_str(), w.model.as_str(), w.session.as_str()),
            ("pi", "opus", "s1")
        );
        assert_eq!(
            with(&[("AI_SESSION", "s2"), ("AI_AGENT", "")]).session,
            "s2"
        );
    }

    fn write(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    fn assistant(model: &str) -> String {
        format!(r#"{{"type":"assistant","message":{{"model":"{model}","role":"assistant"}}}}"#)
    }

    fn padding(n: usize) -> Vec<String> {
        let line = format!(
            r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
            "x".repeat(1000)
        );
        vec![line; n]
    }

    #[test]
    fn claude_models() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_str().unwrap();
        let mut lines = vec![assistant("claude-old"), assistant("claude-opus-5-5")];
        lines.push(assistant("<synthetic>"));
        lines.extend(padding(200));
        write(
            &home.path().join(".claude/projects/-elsewhere/s-1.jsonl"),
            &lines,
        );
        write(
            &home.path().join(".claude/projects/-other/s-2.jsonl"),
            &[r#"{"type":"user","message":{"model":"x"}}"#.into()],
        );
        let base = [("CLAUDECODE", "1"), ("HOME", h)];
        let model = |id: &str| with(&[base[0], base[1], ("CLAUDE_CODE_SESSION_ID", id)]).model;
        assert_eq!(model("s-1"), "claude-opus-5-5");
        assert_eq!(model("s-2"), "");
        assert_eq!(model("missing"), "");
        assert_eq!(model("../s-1"), "");
        let w = with(&[
            base[0],
            base[1],
            ("CLAUDE_CODE_SESSION_ID", "s-1"),
            ("AI_MODEL", "override"),
        ]);
        assert_eq!(w.model, "override");
        let cfg = home.path().join(".claude");
        let w = with(&[
            ("CLAUDE_CONFIG_DIR", cfg.to_str().unwrap()),
            ("CLAUDE_CODE_SESSION_ID", "s-1"),
        ]);
        assert_eq!(w.model, "claude-opus-5-5");
    }

    #[test]
    fn codex_models() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_str().unwrap();
        let ctx = |m: &str| format!(r#"{{"type":"turn_context","payload":{{"model":"{m}"}}}}"#);
        write(
            &home
                .path()
                .join(".codex/sessions/2026/10/05/rollout-2026-10-05T17-24-56-t-1.jsonl"),
            &[ctx("gpt-a"), ctx("gpt-b")],
        );
        let w = with(&[("HOME", h), ("CODEX_THREAD_ID", "t-1")]);
        assert_eq!(
            (w.agent.as_str(), w.model.as_str(), w.session.as_str()),
            ("codex", "gpt-b", "t-1")
        );
        assert_eq!(with(&[("HOME", h), ("CODEX_THREAD_ID", "t-2")]).model, "");
    }

    #[test]
    fn tails_grow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut lines = vec![assistant("first")];
        lines.extend(padding(2000));
        write(&path, &lines);
        assert_eq!(
            last_model(&path, r#""type":"assistant""#, "/message/model").as_deref(),
            Some("first")
        );
    }

    #[test]
    fn repo_names() {
        for (url, want) in [
            ("git@github.com:acme/widget.git", Some("acme/widget")),
            ("https://github.com/acme/widget", Some("acme/widget")),
            (
                "https://gitlab.com/group/sub/widget.git/",
                Some("sub/widget"),
            ),
            ("/srv/git/widget.git", Some("git/widget")),
            ("widget", None),
        ] {
            assert_eq!(owner_name(url.into()).as_deref(), want, "{url}");
        }
    }
}
