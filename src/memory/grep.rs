use std::{collections::VecDeque, io::Write, process::ExitCode};

use anyhow::{Result, anyhow};
use clap::Args;
use regex::{Regex, RegexBuilder};

use crate::{
    memory::{
        cli::{line_id, plural},
        meta::{Line, Meta},
        record::{Kind, Message, Who, flat, midnight},
        store::{ME, Snapshot},
        tree::Coord,
    },
    page::PART,
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
        long,
        value_name = "ID+N",
        help = "only older than this line: the next page"
    )]
    before: Option<String>,
    #[arg(
        short = 'm',
        long = "max-count",
        value_name = "N",
        value_parser = clap::value_parser!(u64).range(1..),
        help = "at most N matches per page"
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

    /// The arguments, as typed, but `--before`.
    fn args(&self) -> Vec<String> {
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

    /// Every argument, as typed.
    pub fn typed(&self) -> Vec<String> {
        let mut a = self.args();
        if let Some(b) = &self.before {
            a.extend(["--before".into(), b.clone()]);
        }
        a
    }

    fn command(&self) -> String {
        let a = self.args();
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
}

/// The newest units within a byte and count budget; everything at or below `floor` was dropped.
struct Page {
    units: VecDeque<Unit>,
    size: u64,
    chars: u64,
    max: u64,
    floor: Option<Key>,
}

impl Page {
    fn new(chars: u64, max: Option<u64>) -> Page {
        Page {
            units: VecDeque::new(),
            size: 0,
            chars,
            max: max.unwrap_or(u64::MAX),
            floor: None,
        }
    }

    fn push(&mut self, u: Unit) {
        if self.floor.is_some_and(|f| u.key() <= f) {
            return;
        }
        self.size += u.size;
        self.units.push_back(u);
        self.trim();
    }

    fn extend(&mut self, i: u64, line: Line) {
        let Some(u) = self.units.back_mut() else {
            return;
        };
        u.span = u.span.map(|(lo, _)| (lo, i + 1));
        let n = cost(u.lines.last(), &line);
        u.size += n;
        self.size += n;
        u.lines.push(line);
        self.trim();
    }

    fn trim(&mut self) {
        while self.units.len() > 1 && (self.size > self.chars || self.units.len() as u64 > self.max)
        {
            let u = self.units.pop_front().expect("len > 1");
            self.size -= u.size;
            self.floor = self.floor.max(Some(u.key()));
        }
    }
}

pub fn grep(s: &Snapshot, out: &mut dyn Write, g: &Grep) -> Result<ExitCode> {
    let pat = g.pat.regex()?;
    let t = s.log_len()?;
    let before = g
        .before
        .as_deref()
        .map(|b| line_id(b, t))
        .transpose()?
        .map(key);
    let below = |k: Key| before.is_none_or(|b| k < b);
    let end = before.map_or(t, |b| b.0 + 1);
    let since = g.since.map(midnight).unwrap_or_default();
    let (mut pages, mut hits) = (vec![Page::new(PART, g.max)], 0);

    let log = &mut pages[0];
    let (mut ring, mut after) = (VecDeque::new(), 0);
    s.scan(end, |i, m| {
        if below((i, 1)) && g.admits_message(&since, &m) && pat.is_match(&m.text) {
            hits += 1;
            let first = ring.front().map_or(i, |&(j, _)| j);
            let mut lines: Vec<_> = ring.drain(..).map(|(_, l)| l).collect();
            lines.push(detail(i, &m, &g.meta, Some(&pat)));
            log.push(Unit::new(Coord::leaf(i), Some((first, i + 1)), lines));
            after = g.context;
        } else if after > 0 {
            after -= 1;
            log.extend(i, detail(i, &m, &g.meta, None));
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
        let mut page = Page::new(PART, g.max);
        for (i, n) in s.level(l)?.into_iter().enumerate() {
            let c = Coord::new(l, i as u64);
            let Some(n) = n.filter(|_| c.end() <= end) else {
                continue;
            };
            if below(key(c)) && g.admits(&since, &n.ts, &n.origin, &n.who) && pat.is_match(&n.text)
            {
                hits += 1;
                let line = g.meta.line_at(s, c, &n.text)?;
                page.push(Unit::new(c, None, vec![line]));
            }
        }
        pages.push(page);
        l += 1;
    }

    if hits == 0 {
        writeln!(out, "No match.")?;
        return Ok(ExitCode::SUCCESS);
    }
    if g.count {
        writeln!(out, "{}.", plural(hits, "match"))?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut units: Vec<_> = pages.iter_mut().flat_map(|p| p.units.drain(..)).collect();
    units.sort_unstable_by_key(Unit::key);
    let mut page = Page::new(PART, g.max);
    page.floor = pages.iter().filter_map(|p| p.floor).max();
    for u in units {
        page.push(u);
    }

    let (mut prev, mut last) = (None, None);
    for (k, u) in page.units.iter().enumerate() {
        let joined = prev.is_some_and(|e| u.span.is_some_and(|(lo, _)| lo == e));
        if g.context > 0 && k > 0 && !joined {
            writeln!(out, "--")?;
        }
        for l in &u.lines {
            if last.is_none_or(|p: &Line| p.day != l.day) {
                writeln!(out, "{}", l.day)?;
            }
            writeln!(out, "{}", l.text)?;
            last = Some(l);
        }
        prev = u.span.map(|(_, hi)| hi);
    }
    let shown = page.units.len() as u64;
    match page.units.front() {
        Some(u) if shown < hits => writeln!(
            out,
            "Newest {shown} of {hits}. Older: {} --before {}",
            g.command(),
            u.line
        )?,
        _ => writeln!(out, "{}.", plural(hits, "match"))?,
    }
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
