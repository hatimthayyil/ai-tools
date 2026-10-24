use std::{collections::VecDeque, io::Write, ops::Range, path::Path, process::ExitCode};

use anyhow::{Result, anyhow};
use clap::Args;
use regex::{Regex, RegexBuilder};

use crate::{
    memory::{
        cli::plural,
        meta::{Line, Meta},
        record::{Kind, Message, Who, flat, midnight},
        store::{ME, Store},
        tree::Coord,
    },
    page,
};

const EXCERPT: usize = 400;

/// A search pattern as typed, rg-style: a regex, or a literal with `-F`; smart case.
#[derive(Args, Debug)]
pub struct Pattern {
    #[arg(
        allow_hyphen_values = true,
        help = "a regex, matched against the text only"
    )]
    pattern: String,
    #[arg(short = 'F', long, help = "match the pattern literally")]
    fixed_strings: bool,
    #[arg(short, long, overrides_with = "case_sensitive", help = "ignore case")]
    ignore_case: bool,
    #[arg(
        short = 's',
        long,
        overrides_with = "ignore_case",
        help = "match case (default: only if the pattern has a capital)"
    )]
    case_sensitive: bool,
}

impl Pattern {
    pub fn regex(&self) -> Result<Regex> {
        let src = match self.fixed_strings {
            true => regex::escape(&self.pattern),
            false => self.pattern.clone(),
        };
        let fold = self.ignore_case
            || !self.case_sensitive && !has_upper(&self.pattern, self.fixed_strings);
        RegexBuilder::new(&src)
            .case_insensitive(fold)
            .build()
            .map_err(|e| anyhow!("bad regex: {e}"))
    }

    /// The arguments, as typed.
    pub fn args(&self) -> Vec<String> {
        let flags = [
            (self.fixed_strings, "-F"),
            (self.ignore_case, "-i"),
            (self.case_sensitive, "-s"),
        ];
        std::iter::once(self.pattern.clone())
            .chain(
                flags
                    .iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, f)| f.to_string()),
            )
            .collect()
    }
}

#[derive(Args, Debug)]
pub struct Grep {
    #[command(flatten)]
    pat: Pattern,
    #[arg(short, long, help = "also search the summaries")]
    tree: bool,
    #[arg(long, value_name = "YYYY-MM-DD", help = "only from this day on (UTC)")]
    since: Option<jiff::civil::Date>,
    #[arg(long, value_enum, help = "only messages of this kind")]
    kind: Option<Kind>,
    #[arg(long, help = "only from this store")]
    origin: Option<String>,
    #[arg(long, help = "only by this agent")]
    agent: Option<String>,
    #[arg(long, help = "only from this session")]
    session: Option<String>,
    #[arg(long, value_name = "OWNER/NAME", help = "only noted in this repo")]
    repo: Option<String>,
    #[arg(
        short = 'm',
        long = "max-count",
        value_name = "N",
        value_parser = clap::value_parser!(u64).range(1..),
        help = "only the newest N matches"
    )]
    max: Option<u64>,
    #[arg(
        short = 'C',
        long,
        value_name = "N",
        default_value_t = 0,
        help = "show N neighbouring messages"
    )]
    context: u64,
    #[arg(short, long, help = "print only the count")]
    count: bool,
    #[command(flatten)]
    meta: Meta,
}

type Key = (u64, u64);

fn key(c: Coord) -> Key {
    (c.end() - 1, c.n())
}

/// `s` in single quotes, for a shell.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `s` as one shell word: bare if it needs no quotes.
pub fn word(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "._/:@+=,-".contains(c);
    match !s.is_empty() && s.chars().all(safe) {
        true => s.into(),
        false => quote(s),
    }
}

fn has_upper(pattern: &str, literal: bool) -> bool {
    let mut cs = pattern.chars();
    while let Some(c) = cs.next() {
        if c == '\\' && !literal {
            if cs.next().is_some() && cs.clone().next() == Some('{') {
                cs.find(|&c| c == '}');
            }
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// `text` on one line; past [`EXCERPT`] bytes, a window around the first match.
pub fn excerpt(text: &str, pat: Option<&Regex>) -> String {
    let text = flat(text);
    if text.len() <= EXCERPT {
        return text;
    }
    let at = pat.and_then(|p| p.find(&text)).map_or(0, |m| m.start());
    let lo = text.floor_char_boundary(at.saturating_sub(EXCERPT / 4));
    let hi = text.floor_char_boundary(lo + EXCERPT);
    let (pre, post) = (
        if lo > 0 { "…" } else { "" },
        if hi < text.len() { "…" } else { "" },
    );
    format!("{pre}{}{post}", &text[lo..hi])
}

fn detail(i: u64, m: &Message, meta: &Meta, pat: Option<&Regex>) -> Line {
    meta.line(
        Coord::leaf(i),
        std::slice::from_ref(m),
        &excerpt(&m.label(), pat),
    )
}

impl Grep {
    fn admits(&self, since: &str, ts: &str, origin: &str, who: &Who) -> bool {
        let is = |want: &Option<String>, have: &str| {
            want.as_deref().is_none_or(|w| have.eq_ignore_ascii_case(w))
        };
        ts >= since
            && is(&self.origin, origin)
            && is(&self.agent, &who.agent)
            && is(&self.session, &who.session)
    }

    fn admits_message(&self, since: &str, m: &Message) -> bool {
        let is_repo = self
            .repo
            .as_deref()
            .is_none_or(|r| m.place.repo.eq_ignore_ascii_case(r));
        self.admits(since, &m.ts, &m.origin, &m.who)
            && self.kind.is_none_or(|k| k == m.kind)
            && is_repo
    }

    fn message_only(&self) -> bool {
        self.kind.is_some() || self.repo.is_some()
    }

    /// Every argument, as typed.
    pub fn typed(&self) -> Vec<String> {
        let mut a = self.pat.args();
        let flags = [(self.tree, "-t"), (self.count, "-c")];
        a.extend(
            flags
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, f)| f.to_string()),
        );
        let opts = [
            ("--since", self.since.map(|d| d.to_string())),
            ("--kind", self.kind.map(|k| k.name().into())),
            ("--origin", self.origin.clone()),
            ("--agent", self.agent.clone()),
            ("--session", self.session.clone()),
            ("--repo", self.repo.clone()),
            ("-m", self.max.map(|n| n.to_string())),
            ("-C", (self.context > 0).then(|| self.context.to_string())),
        ];
        for (f, v) in opts {
            if let Some(v) = v {
                a.extend([f.to_string(), v]);
            }
        }
        a.extend(self.meta.args());
        a
    }

    fn command(&self) -> String {
        let a = self.typed();
        let rest: String = a[1..].iter().map(|v| format!(" {}", word(v))).collect();
        format!("{ME} grep {}{rest}", quote(&a[0]))
    }
}

struct Unit {
    line: Coord,
    span: Option<(u64, u64)>,
    lines: Vec<Line>,
    size: u64,
}

/// Bytes `l` adds after `prev`, counting its date header when the day changes.
fn cost(prev: Option<&Line>, l: &Line) -> u64 {
    l.size(prev.is_none_or(|p| p.day != l.day))
}

impl Unit {
    /// `size` counts a header before the first line: at most one more than printed.
    fn new(line: Coord, span: Option<(u64, u64)>, lines: Vec<Line>) -> Unit {
        let size = lines
            .iter()
            .enumerate()
            .map(|(k, l)| cost(k.checked_sub(1).map(|p| &lines[p]), l))
            .sum();
        Unit {
            line,
            span,
            lines,
            size,
        }
    }

    fn key(&self) -> Key {
        key(self.line)
    }

    /// Adds `line`, message `i`, as context after the match.
    fn extend(&mut self, i: u64, line: Line) {
        self.span = self.span.map(|(lo, _)| (lo, i + 1));
        self.size += cost(self.lines.last(), &line);
        self.lines.push(line);
    }
}

/// The memory's matches, oldest first: all of them when `whole`, else in parts newest first,
/// saved in `views` and read on with `ai page`.
pub fn grep(
    store: &Store,
    out: &mut dyn Write,
    g: &Grep,
    whole: bool,
    views: &Path,
) -> Result<ExitCode> {
    let s = &store.snapshot()?;
    let pat = g.pat.regex()?;
    let end = s.log_len()?;
    let since = g.since.map(midnight).unwrap_or_default();
    let mut units: Vec<Unit> = Vec::new();

    let (mut ring, mut after) = (VecDeque::new(), 0);
    s.scan(end, |i, m| {
        if g.admits_message(&since, &m) && pat.is_match(&m.text) {
            let first = ring.front().map_or(i, |&(j, _)| j);
            let mut lines: Vec<_> = ring.drain(..).map(|(_, l)| l).collect();
            lines.push(detail(i, &m, &g.meta, Some(&pat)));
            units.push(Unit::new(Coord::leaf(i), Some((first, i + 1)), lines));
            after = g.context;
        } else if after > 0 {
            after -= 1;
            if let Some(u) = units.last_mut() {
                u.extend(i, detail(i, &m, &g.meta, None));
            }
        } else if g.context > 0 {
            ring.push_back((i, detail(i, &m, &g.meta, None)));
            if ring.len() as u64 > g.context {
                ring.pop_front();
            }
        }
        Ok(())
    })?;

    let mut l = 1;
    while g.tree && !g.message_only() && 1u64 << l <= end {
        for (i, n) in s.level(l)?.into_iter().enumerate() {
            let c = Coord::new(l, i as u64);
            let Some(n) = n.filter(|_| c.end() <= end) else {
                continue;
            };
            if g.admits(&since, &n.ts, &n.origin, &n.who) && pat.is_match(&n.text) {
                let line = g.meta.line_at(s, c, &n.text)?;
                units.push(Unit::new(c, None, vec![line]));
            }
        }
        l += 1;
    }

    let hits = units.len() as u64;
    if hits == 0 {
        writeln!(out, "No match.")?;
        return Ok(ExitCode::SUCCESS);
    }
    if g.count {
        writeln!(out, "{}.", plural(hits, "match"))?;
        return Ok(ExitCode::SUCCESS);
    }
    units.sort_unstable_by_key(Unit::key);
    let kept = &units[units.len() - g.max.map_or(units.len(), |m| units.len().min(m as usize))..];
    let sizes: Vec<u64> = kept.iter().map(|u| u.size).collect();
    let render = |shown: Range<usize>| {
        let (mut text, mut prev, mut last) = (String::new(), None, None);
        for (k, u) in kept[shown].iter().enumerate() {
            let joined = prev.is_some_and(|e| u.span.is_some_and(|(lo, _)| lo == e));
            if g.context > 0 && k > 0 && !joined {
                text += "--\n";
            }
            for l in &u.lines {
                if last.is_none_or(|p: &Line| p.day != l.day) {
                    text += &format!("{}\n", l.day);
                }
                text += &format!("{}\n", l.text);
                last = Some(l);
            }
            prev = u.span.map(|(_, hi)| hi);
        }
        text
    };
    page::search(out, views, &g.command(), &sizes, hits, whole, render)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_case_ignores_escapes() {
        assert!(has_upper("Foo", false));
        assert!(!has_upper(r"\Sfoo\p{Greek}\x{4A}", false));
        assert!(has_upper(r"\Sfoo\p{Greek}A", false));
        assert!(has_upper(r"\S", true));
    }

    #[test]
    fn keys_order_by_last_member_then_size() {
        let k = |l, i| key(Coord::new(l, i));
        assert!(k(1, 1) < k(2, 0) && k(0, 3) < k(1, 1));
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(
            (word("acme/w-1"), word("a b")),
            ("acme/w-1".into(), "'a b'".into())
        );
    }

    #[test]
    fn long_texts_show_a_window_around_the_match() {
        assert_eq!(excerpt("a\nb", None), "a b");
        let long = format!("{}needle{}", "x".repeat(1000), "y".repeat(1000));
        let re = Regex::new("needle").unwrap();
        let e = excerpt(&long, Some(&re));
        assert!(e.starts_with('…') && e.ends_with('…') && e.contains("needle"));
        assert!(e.len() <= EXCERPT + 2 * '…'.len_utf8());
        let head = excerpt(&long, None);
        assert!(head.starts_with("xxx") && head.ends_with('…'));
    }
}
