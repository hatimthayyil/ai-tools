use std::{collections::HashMap, io::Write, path::Path, process::ExitCode};

use anyhow::Result;
use clap::{Args, ValueEnum};
use jiff::{ToSpan, civil::Date, tz::TimeZone};

use crate::memory::{
    cli::plural,
    hook::{PERIODIC, REMINDED},
    meta::{dash, date},
    record::{Kind, midnight},
    store::Snapshot,
    usage::{self, Use},
};

const CMDS: [&str; 5] = ["view", "note", "zoom", "grep", "show"];
const DAYS: i32 = 14;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum By {
    #[default]
    Day,
    Agent,
    Repo,
    Session,
}

impl By {
    fn name(self) -> &'static str {
        match self {
            By::Day => "day",
            By::Agent => "agent",
            By::Repo => "repo",
            By::Session => "session",
        }
    }
}

#[derive(Args, Debug)]
pub struct Stats {
    #[arg(
        long,
        value_name = "YYYY-MM-DD",
        help = "only from this day on (UTC); default: the last 14 days"
    )]
    since: Option<Date>,
    #[arg(
        long,
        value_enum,
        default_value_t,
        help = "a row per day, agent, repo or session"
    )]
    by: By,
}

impl Stats {
    pub fn args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if let Some(d) = self.since {
            a.extend(["--since".into(), d.to_string()]);
        }
        if self.by != By::Day {
            a.extend(["--by".into(), self.by.name().into()]);
        }
        a
    }
}

/// One successful call: when, which of [`CMDS`], by which agent, from which repo and session.
struct Call {
    ts: String,
    cmd: usize,
    agent: String,
    repo: String,
    session: String,
}

impl Call {
    fn key(&self, by: By) -> String {
        match by {
            By::Day => date(&self.ts),
            By::Agent => self.agent.clone(),
            By::Repo => self.repo.clone(),
            By::Session => self.session.clone(),
        }
    }
}

type Counts = [u64; CMDS.len()];

/// Rows keyed by `key`, in order of first call.
fn rows(calls: &[Call], key: impl Fn(&Call) -> String) -> Vec<(String, Counts)> {
    let mut at = HashMap::new();
    let mut rows: Vec<(String, Counts)> = Vec::new();
    for c in calls {
        let k = key(c);
        let i = *at.entry(k.clone()).or_insert_with(|| {
            rows.push((k, Counts::default()));
            rows.len() - 1
        });
        rows[i].1[c.cmd] += 1;
    }
    rows
}

fn table(out: &mut dyn Write, by: By, rows: &[(String, Counts)]) -> Result<()> {
    let head = by.name();
    let w = rows
        .iter()
        .map(|(k, _)| k.len())
        .fold(head.len(), usize::max);
    let mut all = Counts::default();
    let line = |out: &mut dyn Write, k: &str, cs: &[String]| {
        let cs: String = cs.iter().map(|c| format!("{c:>6}")).collect();
        writeln!(out, "{k:<w$}{cs}")
    };
    line(out, head, &CMDS.map(String::from))?;
    for (k, cs) in rows {
        line(out, k, &cs.map(|n| n.to_string()))?;
        for (a, n) in all.iter_mut().zip(cs) {
            *a += n;
        }
    }
    line(out, "all", &all.map(|n| n.to_string()))?;
    Ok(())
}

/// Per session that woke: the mean calls of each other command, and the share that made any.
fn sessions(out: &mut dyn Write, calls: &[Call]) -> Result<()> {
    let per: Vec<Counts> = rows(calls, |c| c.key(By::Session))
        .into_iter()
        .filter(|(k, _)| k != "-")
        .map(|(_, cs)| cs)
        .collect();
    let woke: Vec<&Counts> = per.iter().filter(|cs| cs[0] > 0).collect();
    let n = woke.len();
    write!(out, "\n{}", plural(per.len() as u64, "session"))?;
    if n == 0 {
        writeln!(out, ", none woke.")?;
        return Ok(());
    }
    writeln!(out, ", {n} woke.")?;
    let done = ["noted", "zoomed", "grepped", "showed"];
    let (mut mean, mut share) = (Vec::new(), Vec::new());
    for (i, d) in (1..CMDS.len()).zip(done) {
        let sum: u64 = woke.iter().map(|cs| cs[i]).sum();
        let any = woke.iter().filter(|cs| cs[i] > 0).count();
        mean.push(format!("{:.1} {}s", sum as f64 / n as f64, CMDS[i]));
        share.push(format!("{}% {d}", (200 * any + n) / (2 * n)));
    }
    writeln!(out, "Per woken session: {}.", mean.join(", "))?;
    writeln!(out, "Of these, {}.", share.join(", "))?;
    Ok(())
}

pub fn stats(s: &Snapshot, dir: &Path, out: &mut dyn Write, st: &Stats) -> Result<ExitCode> {
    let since = match st.since {
        Some(d) => d,
        None => jiff::Timestamp::now()
            .to_zoned(TimeZone::UTC)
            .date()
            .checked_sub((DAYS - 1).days())?,
    };
    let from = midnight(since);
    let cmd = |name: &str| CMDS.iter().position(|c| *c == name);
    let mut calls = Vec::new();
    s.scan(s.log_len()?, |_, m| {
        if m.kind == Kind::Note && m.ts >= from {
            calls.push(Call {
                ts: m.ts,
                cmd: 1,
                agent: dash(&m.who.agent).into(),
                repo: dash(&m.place.repo).into(),
                session: dash(&m.who.session).into(),
            });
        }
        Ok(())
    })?;
    let mut failed = Counts::default();
    let (mut hooked, mut reminded, mut periodic) = (0, 0, 0);
    for u in usage::read(dir)?.into_iter().filter(|u| u.ts >= from) {
        if u.cmd == "prompt-hook" && u.ok {
            let has = |a: &str| u64::from(u.args.iter().any(|x| x == a));
            hooked += 1;
            reminded += has(REMINDED);
            periodic += has(PERIODIC);
        }
        let Some(i) = cmd(&u.cmd) else { continue };
        let Use {
            ts,
            ok,
            agent,
            repo,
            session,
            ..
        } = u;
        if !ok {
            failed[i] += 1;
        } else if i != 1 {
            calls.push(Call {
                ts,
                cmd: i,
                agent,
                repo,
                session,
            });
        }
    }
    calls.sort_by(|a, b| a.ts.cmp(&b.ts));
    writeln!(out, "Since {since}, UTC.")?;
    if calls.is_empty() && failed.iter().all(|&n| n == 0) {
        writeln!(out, "No calls.")?;
        return Ok(ExitCode::SUCCESS);
    }
    table(out, st.by, &rows(&calls, |c| c.key(st.by)))?;
    sessions(out, &calls)?;
    if hooked > 0 {
        writeln!(
            out,
            "{} hooked, {reminded} reminded ({periodic} periodic).",
            plural(hooked, "prompt")
        )?;
    }
    let failed: Vec<String> = CMDS
        .iter()
        .zip(failed)
        .filter(|(_, n)| *n > 0)
        .map(|(c, n)| plural(n, c))
        .collect();
    if !failed.is_empty() {
        writeln!(out, "Failed: {}.", failed.join(", "))?;
    }
    Ok(ExitCode::SUCCESS)
}
