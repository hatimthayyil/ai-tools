use std::{collections::HashMap, ops::Range};

use anyhow::Result;
use regex::Regex;

use crate::{
    chat::{
        id::Id,
        model::{Item, Kind, Tree, Turn},
        record::{Binding, Key},
        render::{Cut, WHOLE, item_line, searched},
        rollup,
        store::Snapshot,
    },
    memory::{Coord, excerpt, flat},
};

/// The most lines the fold prints.
pub const FOLD_LINES: usize = 64;
/// The most the user rail prints, in bytes.
pub const RAIL: usize = 8_000;
/// The most the open turn prints, in bytes.
pub const OPEN: usize = 16_000;
/// The most of a message's words the fold prints for a turn not summarised yet.
pub const LINE: usize = 512;
/// The most of one message's words a listing prints.
const WORDS: usize = 8_000;
/// The most of one tool call or output the open turn prints.
const TOOL: usize = 2_000;
/// The most of a tool call's input a zoom lists, on one line.
const CALL: usize = 400;

const FOLD: &str = "Earlier turns, summarised";
const USER: &str = "The user's latest messages";
const CURRENT: &str = "The turn in progress";
const PREVIOUS: &str = "The previous session's last turn";

const INSTRUCTION: &str = "\
This is the thread `{name}`: your conversation so far, oldest first, in place of the context \
you had. When two lines disagree, trust the later one.

A turn is one message from the user or another agent, plus everything you did to answer it. \
Turns count from 0, and so do the items of a turn; item 0 is the turn's message. Each line is \
`id|text`:
- `12|` summarises turn 12; `8+4|` summarises turns 8 to 11.
- `12.3|` is item 3 of turn 12; `12.3+7|` summarises its items 3 to 9.
An item's text continues on lines indented by two spaces. Summaries cite the item that holds a \
detail, as [12.3].

Zoom a line to see what it was made from: `ai chat zoom 8+4`. You can zoom any turn, item or \
range. Zoom lists tool calls without their output; add `--tool-output` to include it. \
`ai chat grep <regex>` searches the thread and takes the same flag. Long output comes in parts; \
each part ends with the command for the next.

Summaries keep little tool output, so state in your replies what you learned that you will \
need later.
";

/// The fold over `t` closed turns: from one line each, merges the most due aligned pair,
/// due = (t - last)/2^l, oldest first on a tie, until the lines' `cost` fits `budget`.
pub fn fold(t: u64, budget: usize, cost: impl Fn(Coord) -> usize) -> Vec<Coord> {
    let mut view: Vec<Coord> = (0..t).map(Coord::leaf).collect();
    let mut size: usize = view.iter().map(|&c| cost(c)).sum();
    while size > budget {
        let mut best: Option<(usize, f64)> = None;
        for (k, w) in view.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            if a.l != b.l || a.i % 2 != 0 || b.i != a.i + 1 {
                continue;
            }
            let due = (t - (b.end() - 1)) as f64 / b.n() as f64;
            if best.is_none_or(|(_, d)| due > d) {
                best = Some((k, due));
            }
        }
        let Some((k, _)) = best else {
            break;
        };
        let p = view[k].parent();
        size = size + cost(p) - cost(view[k]) - cost(view[k + 1]);
        view.splice(k..k + 2, [p]);
    }
    view
}

/// The nodes `d` levels under `c`, in order.
fn under(c: Coord, d: u32) -> Vec<Coord> {
    let l = c.l - d;
    (c.i << d..(c.i + 1) << d)
        .map(|i| Coord::new(l, i))
        .collect()
}

/// A match of `Branch::grep`: where it ends on the path, then how many positions it spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub id: Id,
    pub order: (usize, usize),
    pub line: String,
}

/// The order of a match over positions `r`: by its last position, then by its size.
fn order(r: &Range<usize>) -> (usize, usize) {
    (r.end - 1, r.len())
}

/// The fewest aligned nodes that tile turns `first` to `end - 1`, in order.
fn tile(first: u64, end: u64) -> Vec<Coord> {
    let mut out = Vec::new();
    let mut at = first;
    while at < end {
        let mut l = 0;
        while at.is_multiple_of(2 << l) && at + (2 << l) <= end {
            l += 1;
        }
        out.push(Coord::new(l, at >> l));
        at += 1 << l;
    }
    out
}

/// A session's branch of a thread: its items from the root to its tip, cut into turns, with
/// the lines built over them. Coordinates of the fold count turns along the branch: (l, i)
/// covers turns [i·2^l, (i+1)·2^l), the line of level 1+l.
pub struct Branch {
    tree: Tree,
    /// The sequence numbers from the root to the tip, increasing.
    path: Vec<usize>,
    xs: Vec<Turn>,
    /// Each turn's chunks, as the roll-up cuts it.
    cuts: Vec<Vec<Range<usize>>>,
    lines: HashMap<Key, String>,
    /// A session with no item of its own: its last turn is the previous session's.
    seeded: bool,
}

impl Branch {
    /// The branch ending at item `tip`, none for no item, with `lines` by key.
    pub fn new(tree: Tree, tip: Option<u64>, lines: HashMap<Key, String>) -> Branch {
        let b = tip.map(|t| rollup::branch(&tree, t as usize));
        let (path, parts) = b.map_or_else(Default::default, |b| (b.seqs, b.parts));
        let (xs, cuts) = parts.into_iter().map(|p| (p.turn, p.chunks)).unzip();
        Branch {
            tree,
            path,
            xs,
            cuts,
            lines,
            seeded: false,
        }
    }

    /// The branch of `s` ending at item `tip`, with every line of the thread.
    pub fn load(s: &Snapshot, tip: Option<u64>) -> Result<Branch> {
        let lines = s
            .all_lines()?
            .into_iter()
            .map(|(k, l)| (k, l.text))
            .collect();
        Ok(Branch::new(s.tree()?, tip, lines))
    }

    /// The branch of session `b`: to its tip, else to its graft, the thread's newest item when
    /// it was attached.
    pub fn session(s: &Snapshot, b: &Binding) -> Result<Branch> {
        let mut branch = Branch::load(s, b.tip.or(b.graft))?;
        branch.seeded = b.tip.is_none();
        Ok(branch)
    }

    /// The view: the instruction, the fold over the closed turns in at most [`FOLD_LINES`]
    /// lines, the user rail and the open turn.
    pub fn view(&self, thread: &str) -> String {
        let mut head = format!(
            "# Thread `{thread}`\n\n{}",
            INSTRUCTION.replace("{name}", thread)
        );
        if self.xs.is_empty() {
            head += "\nThe thread holds nothing yet.\n";
        }
        let fold = section(FOLD, &self.render());
        let rail = section(USER, &self.rail());
        let last = if self.seeded { PREVIOUS } else { CURRENT };
        let open = section(last, &self.current());
        format!("{head}{fold}{rail}{open}")
    }

    /// The keys of the lines the fold wants and lacks.
    pub fn needed(&self) -> Vec<Key> {
        self.plan()
            .into_iter()
            .filter(|&c| self.text(c).is_none())
            .map(|c| self.key(c))
            .collect()
    }

    /// What `id` holds, one or two levels down: a node into the lines two levels under it
    /// (`t+2` into its two turn lines), a turn into its chunk lines and items, items into the
    /// chunk lines inside them and their other items, one item whole; turns that are no node
    /// into the fewest nodes that tile them. A line not built shows what is built under it.
    /// Tool output only with `tool_output`. `None` if `id` is not on this branch.
    pub fn zoom(&self, id: Id, tool_output: bool) -> Option<String> {
        let listing = match tool_output {
            true => Cut::WHOLE,
            false => Cut {
                words: WHOLE,
                input: CALL,
                output: None,
            },
        };
        let r = self.positions(id)?;
        let lines = match id {
            Id::Turns { first, n } => {
                let nodes = match Coord::at(first, n, first + n) {
                    Some(c) if c.l == 0 => return Some(self.inside(r, listing).join("\n")),
                    Some(c) => under(c, c.l.min(2)),
                    None => tile(first, first + n),
                };
                let mut out = Vec::new();
                for c in nodes {
                    self.push(c, &mut out);
                }
                out
            }
            Id::Items { n: 1, .. } => vec![self.item_line(
                r.start,
                Cut {
                    output: tool_output.then_some(WHOLE),
                    ..Cut::WHOLE
                },
            )],
            Id::Items { .. } => self.inside(r, listing),
        };
        Some(lines.join("\n"))
    }

    /// The matches of `re` on this branch, oldest first: the words of messages and replies,
    /// tool calls, tool output only with `tool_output`, and the lines built over the branch.
    /// `before` keeps those that end before it, or are smaller and end with it.
    pub fn grep(&self, re: &Regex, tool_output: bool, before: Option<Id>) -> Option<Vec<Hit>> {
        let below = match before {
            Some(id) => order(&self.positions(id)?),
            None => (usize::MAX, 0),
        };
        let mut hits = Vec::new();
        for p in 0..self.path.len() {
            if let Some((label, text)) = searched(self.item(p), tool_output)
                && re.is_match(&text)
            {
                let id = self.items_id(&(p..p + 1));
                let line = format!("{id}|{label}: {}", excerpt(&text, Some(re)));
                hits.push((id, p..p + 1, line));
            }
        }
        let mut lines: Vec<(Id, Range<usize>, &str)> = Vec::new();
        for e in 0..self.xs.len() {
            for (r, t) in self.chunks(e) {
                lines.push((self.items_id(&r), r, t));
            }
        }
        let closed = self.closed();
        for l in 0..u64::BITS - closed.leading_zeros() {
            for i in 0..closed >> l {
                let c = Coord::new(l, i);
                if let Some(t) = self.text(c) {
                    lines.push((Id::node(c), self.span(c), t));
                }
            }
        }
        for (id, r, t) in lines {
            if re.is_match(t) {
                hits.push((id, r, format!("{id}|{}", excerpt(t, Some(re)))));
            }
        }
        let mut hits: Vec<Hit> = hits
            .into_iter()
            .map(|(id, r, line)| Hit {
                id,
                order: order(&r),
                line,
            })
            .filter(|h| h.order < below)
            .collect();
        hits.sort_by_key(|h| h.order);
        Some(hits)
    }

    /// The key of the line `id` names on this branch: a closed turn, a node over closed turns,
    /// or a chunk that no longer grows; `None` if it names no line.
    pub fn line_key(&self, id: Id) -> Option<Key> {
        match id {
            Id::Turns { first, n } => Some(self.key(Coord::at(first, n, self.closed())?)),
            Id::Items { turn, .. } => {
                let r = self.positions(id)?;
                let cuts = &self.cuts[turn as usize];
                let at = cuts.iter().position(|c| *c == r)?;
                let open = turn == self.closed() && at + 1 == cuts.len();
                (!open).then(|| Key {
                    level: 0,
                    last: self.path[r.end - 1] as u64,
                })
            }
        }
    }

    /// The positions on the path `id` names; `None` if it is not on this branch.
    fn positions(&self, id: Id) -> Option<Range<usize>> {
        match id {
            Id::Turns { first, n } => {
                let end = first.checked_add(n)?;
                (end <= self.xs.len() as u64)
                    .then(|| self.xs[first as usize].span.start..self.xs[end as usize - 1].span.end)
            }
            Id::Items { turn, first, n } => {
                let x = self.xs.get(turn as usize)?;
                let a = x.span.start.checked_add(first as usize)?;
                let b = a.checked_add(n as usize)?;
                (b <= x.span.end).then_some(a..b)
            }
        }
    }

    fn closed(&self) -> u64 {
        self.xs.len().saturating_sub(1) as u64
    }

    fn item(&self, p: usize) -> &Item {
        &self.tree.items()[self.path[p]]
    }

    /// The id of the items at positions `r`, all in one turn.
    fn items_id(&self, r: &Range<usize>) -> Id {
        let p = self.tree.place(self.path[r.start]);
        Id::Items {
            turn: p.turn,
            first: p.item,
            n: r.len() as u64,
        }
    }

    /// The positions on the path of the turns `c` covers.
    fn span(&self, c: Coord) -> Range<usize> {
        self.xs[c.id() as usize].span.start..self.xs[c.end() as usize - 1].span.end
    }

    fn key(&self, c: Coord) -> Key {
        Key {
            level: 1 + c.l,
            last: self.path[self.span(c).end - 1] as u64,
        }
    }

    fn text(&self, c: Coord) -> Option<&str> {
        self.lines.get(&self.key(c)).map(String::as_str)
    }

    fn turn_at(&self, p: usize) -> usize {
        self.xs.partition_point(|x| x.span.end <= p)
    }

    /// Where turn `e`'s steps start: after its message.
    fn body(&self, e: usize) -> usize {
        let s = self.xs[e].span.start;
        s + usize::from(self.item(s).kind.opens())
    }

    /// The chunk lines built in turn `e`, consecutive from its first step, each with the
    /// positions it covers.
    fn chunks(&self, e: usize) -> Vec<(Range<usize>, &str)> {
        self.cuts[e]
            .iter()
            .map_while(|r| {
                let key = Key {
                    level: 0,
                    last: self.path[r.end - 1] as u64,
                };
                Some((r.clone(), self.lines.get(&key)?.as_str()))
            })
            .collect()
    }

    /// Item `p` as a line, cut by `cut`.
    fn item_line(&self, p: usize, cut: Cut) -> String {
        let id = Id::item(self.tree.place(self.path[p]));
        item_line(id, self.item(p), cut)
    }

    fn line(id: Id, text: &str) -> String {
        format!("{id}|{}", flat(text))
    }

    fn placeholder(id: Id) -> String {
        format!("{id}|(not summarised yet)")
    }

    /// The lines the fold wants: at most [`FOLD_LINES`].
    fn plan(&self) -> Vec<Coord> {
        fold(self.closed(), FOLD_LINES, |_| 1)
    }

    /// The fold's lines: each built line of the plan, else what is built under it; the oldest
    /// of those fallbacks become placeholders while over [`FOLD_LINES`].
    fn render(&self) -> Vec<String> {
        let plan = self.plan();
        let mut parts: Vec<Vec<String>> = plan
            .iter()
            .map(|&c| {
                let mut out = Vec::new();
                self.push(c, &mut out);
                out
            })
            .collect();
        let mut n: usize = parts.iter().map(Vec::len).sum();
        for (part, &c) in parts.iter_mut().zip(&plan) {
            if n <= FOLD_LINES {
                break;
            }
            if part.len() > 1 {
                n = n + 1 - part.len();
                *part = vec![Branch::placeholder(Id::node(c))];
            }
        }
        parts.concat()
    }

    /// Line `c`, else the lines built under it, else a placeholder naming it.
    fn push(&self, c: Coord, out: &mut Vec<String>) {
        match self.text(c) {
            Some(t) => out.push(Branch::line(Id::node(c), t)),
            None if !self.built_under(c) => out.push(Branch::placeholder(Id::node(c))),
            None if c.l > 0 => {
                for k in c.children() {
                    self.push(k, out);
                }
            }
            None => {
                let e = c.i as usize;
                let span = self.span(c);
                let body = self.body(e);
                out.extend((span.start..body).map(|p| self.item_line(p, cut(LINE))));
                let chunks = self.chunks(e);
                out.extend(
                    chunks
                        .iter()
                        .map(|(r, t)| Branch::line(self.items_id(r), t)),
                );
                let rest = chunks.last().map_or(body, |c| c.0.end)..span.end;
                if !rest.is_empty() {
                    out.push(Branch::placeholder(self.items_id(&rest)));
                }
            }
        }
    }

    fn built_under(&self, c: Coord) -> bool {
        match c.l {
            0 => !self.chunks(c.i as usize).is_empty(),
            _ => c
                .children()
                .iter()
                .any(|&k| self.text(k).is_some() || self.built_under(k)),
        }
    }

    /// The newest messages of the user in closed turns, oldest first, in about [`RAIL`]
    /// bytes; the newest always.
    fn rail(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut size = 0;
        for x in self.xs[..self.closed() as usize].iter().rev() {
            let p = x.span.start;
            if !matches!(self.item(p).kind, Kind::User(_)) {
                continue;
            }
            let l = self.item_line(p, cut(RAIL));
            if !out.is_empty() && size + l.len() + 1 > RAIL {
                break;
            }
            size += l.len() + 1;
            out.push(l);
        }
        out.reverse();
        out
    }

    /// The open turn: its message, then its chunk lines and the steps after them, the
    /// newest that fit in [`OPEN`] bytes; the newest item always.
    fn current(&self) -> Vec<String> {
        let Some(e) = self.xs.len().checked_sub(1) else {
            return Vec::new();
        };
        let x = &self.xs[e];
        let body = self.body(e);
        let mut out: Vec<String> = (x.span.start..body)
            .map(|p| self.item_line(p, cut(WORDS)))
            .collect();
        let chunks = self.chunks(e);
        let rest = chunks.last().map_or(body, |c| c.0.end);
        let mut units: Vec<(Range<usize>, Vec<String>)> = chunks
            .iter()
            .map(|(r, t)| (r.clone(), vec![Branch::line(self.items_id(r), t)]))
            .collect();
        for s in &x.steps {
            let r = s.start.max(rest)..s.end;
            if !r.is_empty() {
                let lines = r.clone().map(|p| self.item_line(p, cut(WORDS))).collect();
                units.push((r, lines));
            }
        }
        let mut size = bytes(&out);
        let mut keep = units.len();
        while keep > 0 {
            let (r, lines) = &units[keep - 1];
            let b = bytes(lines);
            if size + b > OPEN {
                if keep < units.len() {
                    break;
                }
                if lines.len() > 1 {
                    let items: Vec<_> = r
                        .clone()
                        .zip(lines.clone())
                        .map(|(p, l)| (p..p + 1, vec![l]))
                        .collect();
                    units.splice(keep - 1..keep, items);
                    keep = units.len();
                    continue;
                }
            }
            size += b;
            keep -= 1;
        }
        if keep > 0 {
            let id = self.items_id(&(units[0].0.start..units[keep - 1].0.end));
            out.push(format!("{id}|(not shown)"));
        }
        out.extend(units.drain(keep..).flat_map(|u| u.1));
        out
    }

    /// What `r`, within one turn, holds: the chunk lines inside it other than itself, and
    /// its other items, cut by `cut`.
    fn inside(&self, r: Range<usize>, cut: Cut) -> Vec<String> {
        let chunks: Vec<_> = self
            .chunks(self.turn_at(r.start))
            .into_iter()
            .filter(|(c, _)| r.start <= c.start && c.end <= r.end && *c != r)
            .collect();
        let mut out = Vec::new();
        let mut cs = chunks.iter().peekable();
        let mut p = r.start;
        while p < r.end {
            match cs.next_if(|c| c.0.start == p) {
                Some((c, t)) => {
                    out.push(Branch::line(self.items_id(c), t));
                    p = c.end;
                }
                None => {
                    out.push(self.item_line(p, cut));
                    p += 1;
                }
            }
        }
        out
    }
}

/// An item's words cut to `words` bytes, and its tool input and output to [`TOOL`].
fn cut(words: usize) -> Cut {
    Cut {
        words,
        input: words.min(TOOL),
        output: Some(words.min(TOOL)),
    }
}

/// The bytes `lines` take, one newline each.
fn bytes(lines: &[String]) -> usize {
    lines.iter().map(|l| l.len() + 1).sum()
}

fn section(title: &str, lines: &[String]) -> String {
    match lines.is_empty() {
        true => String::new(),
        false => format!("\n## {title}\n{}\n", lines.join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use serde_json::Value;

    use super::*;
    use crate::chat::model::{Marker, SourceId};

    fn source(entry: usize) -> SourceId {
        SourceId {
            harness: "test".into(),
            session: "s".into(),
            entry: entry.to_string(),
        }
    }

    /// Items in one chain, from kinds: `u` user, `w` work, `t` text, `c` call, `r` result,
    /// each saying `text`.
    fn chain(spec: &str, text: impl Fn(usize) -> String) -> Vec<Item> {
        spec.chars()
            .enumerate()
            .map(|(i, k)| Item {
                id: source(i),
                parent: i.checked_sub(1).map(source),
                ts: Timestamp::UNIX_EPOCH,
                kind: match k {
                    'u' => Kind::User(text(i)),
                    'w' => Kind::Work {
                        from: "researcher".into(),
                        text: text(i),
                    },
                    't' => Kind::Text(text(i)),
                    'c' => Kind::ToolCall {
                        id: i.to_string(),
                        name: "Bash".into(),
                        input: Value::String(text(i)),
                    },
                    'r' => Kind::ToolResult {
                        id: i.to_string(),
                        output: text(i),
                        is_error: false,
                    },
                    _ => Kind::Marker(Marker::Interrupt),
                },
            })
            .collect()
    }

    fn branch(items: Vec<Item>, lines: &[(u32, u64, &str)]) -> Branch {
        let tip = items.len().checked_sub(1).map(|t| t as u64);
        let lines = lines
            .iter()
            .map(|&(level, last, t)| (Key { level, last }, t.to_string()))
            .collect();
        Branch::new(Tree::new(items), tip, lines)
    }

    fn short(i: usize) -> String {
        format!("m{i}")
    }

    /// The lines of one section of `view`.
    fn section_lines<'v>(view: &'v str, title: &str) -> Vec<&'v str> {
        let Some((_, rest)) = view.split_once(&format!("\n## {title}\n")) else {
            return Vec::new();
        };
        rest.split("\n\n## ").next().unwrap().lines().collect()
    }

    fn ids(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.split('|').next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_fold_merges_the_most_due_pair_first() {
        let names = |v: Vec<Coord>| -> Vec<String> { v.iter().map(|c| c.to_string()).collect() };
        assert_eq!(
            names(fold(10, 600, |_| 100)),
            ["0+4", "4+2", "6", "7", "8", "9"]
        );
        assert_eq!(
            names(fold(10, 300, |_| 100)),
            ["0+4", "4+4", "8+2"],
            "due counts from a pair's last turn: 8-9 merges before 0-7"
        );
        assert_eq!(names(fold(10, 1000, |_| 100)).len(), 10);
        assert_eq!(
            names(fold(3, 0, |_| 100)),
            ["0+2", "2"],
            "stops when no pair is left"
        );
        assert!(fold(0, 0, |_| 100).is_empty());
    }

    #[test]
    fn ranges_tile_with_the_fewest_nodes() {
        let names = |v: Vec<Coord>| -> Vec<String> { v.iter().map(|c| c.to_string()).collect() };
        assert_eq!(names(tile(5, 8)), ["5", "6+2"]);
        assert_eq!(names(tile(0, 7)), ["0+4", "4+2", "6"]);
        assert_eq!(names(tile(3, 13)), ["3", "4+4", "8+4", "12"]);
        assert_eq!(names(tile(8, 16)), ["8+8"]);
        assert_eq!(
            names(under(Coord::new(3, 1), 2)),
            ["8+2", "10+2", "12+2", "14+2"]
        );
        assert_eq!(names(under(Coord::new(1, 2), 1)), ["4", "5"]);
    }

    #[test]
    fn built_lines_fill_the_fold_and_the_rest_fall_back() {
        let b = branch(chain("utututututututut", short), &[]);
        assert_eq!(b.closed(), 7);
        let all = b.render();
        assert_eq!(all.len(), 7);
        assert_eq!(all[0], "0|(not summarised yet)");
        let b = branch(
            chain("utututututututut", short),
            &[(1, 1, "first"), (1, 3, "second"), (1, 7, "fourth")],
        );
        assert_eq!(
            b.render(),
            [
                "0|first",
                "1|second",
                "2|(not summarised yet)",
                "3|fourth",
                "4|(not summarised yet)",
                "5|(not summarised yet)",
                "6|(not summarised yet)",
            ],
        );
        let key = |last| Key { level: 1, last };
        assert_eq!(
            b.needed(),
            [key(5), key(9), key(11), key(13)],
            "every turn fits: the fold wants the unbuilt turn lines"
        );
    }

    #[test]
    fn the_fold_keeps_to_its_lines() {
        // 66 closed turns: the two oldest pairs merge, 0+2 and 2+2.
        let b = branch(
            chain(&"ut".repeat(67), short),
            &[(2, 3, "first two"), (1, 5, "third"), (1, 131, "newest")],
        );
        let fold = b.render();
        assert_eq!(fold.len(), FOLD_LINES);
        assert_eq!(
            fold[..3],
            [
                "0+2|first two",
                "2+2|(not summarised yet)",
                "4|(not summarised yet)"
            ],
            "2+2 would fall back to two lines, one too many: it stays one"
        );
        assert_eq!(fold.last().unwrap(), "65|newest");
        let key = |level, last| Key { level, last };
        assert_eq!(b.needed()[..2], [key(2, 7), key(1, 9)]);
        assert_eq!(b.needed().len(), FOLD_LINES - 2);
    }

    /// Item `i` says `m<i>`, padded to about `pad(i)` bytes.
    fn padded(pad: impl Fn(usize) -> usize) -> impl Fn(usize) -> String {
        move |i| format!("m{i}{}", " x".repeat(pad(i) / 2))
    }

    #[test]
    fn an_unbuilt_turn_shows_its_message_and_chunks() {
        // Three steps of a 20 KB reply: over the roll-up's 48 KB input, cut 1-6 and 7-9.
        let text = padded(|i| if [1, 4, 7].contains(&i) { 20_000 } else { 0 });
        let render =
            |lines: &[(u32, u64, &str)]| branch(chain("utcrtcrtcrut", &text), lines).render();
        assert_eq!(
            render(&[(0, 6, "read the config")]),
            [
                "0.0|user: m0",
                "0.1+6|read the config",
                "0.7+3|(not summarised yet)",
            ]
        );
        assert_eq!(
            render(&[(0, 6, "read the config"), (0, 9, "ran the tests")]),
            [
                "0.0|user: m0",
                "0.1+6|read the config",
                "0.7+3|ran the tests"
            ]
        );
        assert_eq!(
            render(&[(0, 9, "ran the tests")]),
            ["0|(not summarised yet)"],
            "chunk lines count from the first"
        );
    }

    /// Level-1 lines over each of `n` two-item turns, each `text`.
    fn turn_lines(n: u64, text: &str) -> Vec<(u32, u64, String)> {
        (0..n)
            .map(|e| (1, 2 * e + 1, format!("{e} {text}")))
            .collect()
    }

    fn as_refs(lines: &[(u32, u64, String)]) -> Vec<(u32, u64, &str)> {
        lines.iter().map(|(l, k, t)| (*l, *k, t.as_str())).collect()
    }

    #[test]
    fn the_view_keeps_its_size() {
        let spec = "ut".repeat(3000) + "u";
        let lines = turn_lines(3000, &"y".repeat(500));
        let big = |i: usize| format!("{i} {}", "z".repeat(20_000));
        for b in [
            branch(chain(&spec, short), &as_refs(&lines)),
            branch(chain(&spec, big), &as_refs(&lines)),
            branch(chain(&spec, big), &[]),
        ] {
            let v = b.view("dev");
            let most = FOLD_LINES * 600 + RAIL + OPEN + 4_000;
            assert!(v.len() <= most, "{}", v.len());
            assert!(v.starts_with("# Thread `dev`\n\nThis is the thread `dev`"));
            let fold = section_lines(&v, FOLD);
            assert!(
                fold.len() > 10 && fold.len() <= FOLD_LINES,
                "{}",
                fold.len()
            );
            assert!(!section_lines(&v, USER).is_empty());
            assert!(section_lines(&v, CURRENT)[0].starts_with("3000.0|user: "));
        }
        let empty = branch(Vec::new(), &[]).view("dev");
        assert!(empty.ends_with("The thread holds nothing yet.\n"));
    }

    #[test]
    fn a_seeded_session_heads_its_last_turn_as_the_previous_session_s() {
        let mut b = branch(chain("ututu", short), &[]);
        assert_eq!(section_lines(&b.view("dev"), CURRENT), ["2.0|user: m4"]);
        b.seeded = true;
        let v = b.view("dev");
        assert!(section_lines(&v, CURRENT).is_empty());
        assert_eq!(section_lines(&v, PREVIOUS), ["2.0|user: m4"]);
    }

    #[test]
    fn the_open_turn_keeps_its_message_and_newest_steps() {
        let text = |i: usize| match i {
            2 => "run the benchmarks".to_string(),
            _ => format!("{i}:{}", "o".repeat(9_000)),
        };
        let open = branch(chain("utuctcrcrcrtcrt", text), &[]).current();
        assert_eq!(open[0], "1.0|user: run the benchmarks");
        assert_eq!(open[1], "1.1+11|(not shown)");
        assert_eq!(ids(&open[2..]), ["1.12"], "whole steps");
        assert!(open[2].starts_with("1.12|ai: 14:") && open[2].contains("bytes cut]"));
        assert!(bytes(&open) <= OPEN);

        let flood = "u".to_string() + &"c".repeat(40) + &"r".repeat(40);
        let open = branch(chain(&flood, text), &[]).current();
        assert!(bytes(&open) <= OPEN, "{}", bytes(&open));
        assert!(open[1].starts_with("0.1+") && open[1].ends_with("|(not shown)"));
        assert!(
            open.last().unwrap().starts_with("0.80|output: "),
            "a step too big for the budget keeps its newest items"
        );

        // Steps of 20 KB, 20 KB, 20 KB and a short one: chunks 1-6 and 7-12, the second open.
        let text = padded(|i| if [1, 4, 7].contains(&i) { 20_000 } else { 0 });
        let lines = [(0, 6, "read it")];
        let open = branch(chain("utcrtcrtcrtcr", text), &lines).current();
        assert_eq!(
            ids(&open),
            ["0.0", "0.1+6", "0.7", "0.8", "0.9", "0.10", "0.11", "0.12"]
        );
        assert_eq!(open[1], "0.1+6|read it");
    }

    #[test]
    fn the_rail_holds_the_newest_user_messages() {
        let text = |i: usize| match i % 4 {
            0 => format!("ask {i} {}", "q".repeat(1_500)),
            _ => format!("say {i}"),
        };
        let rail = branch(chain(&"utwt".repeat(20), text), &[]).rail();
        assert_eq!(rail.len(), 5);
        assert!(rail[0].starts_with("30.0|user: ask 60"));
        assert!(rail[4].starts_with("38.0|user: ask 76"));
        let huge = |i: usize| format!("{i}{}", "q".repeat(20_000));
        let rail = branch(chain("utu", huge), &[]).rail();
        assert_eq!(rail.len(), 1, "the newest always, cut");
        assert!(rail[0].len() < RAIL + 100 && rail[0].contains("bytes cut]"));
    }

    #[test]
    fn zoom_opens_nodes_turns_chunks_and_items() {
        // Turn 2 (items 4-8) holds two steps of about 28 and 24 KB: over 48 KB, cut 5-7 and 8.
        let text = padded(|i| match i {
            5 | 8 => 24_000,
            7 => 10_000,
            _ => 0,
        });
        let b = branch(
            chain("utututcrtutut", &text),
            &[
                (1, 1, "one"),
                (1, 3, "two"),
                (2, 3, "one and two"),
                (0, 7, "ran"),
            ],
        );
        let zoom = |id: &str, out: bool| b.zoom(Id::parse(id).unwrap(), out);
        let heads = |id: &str| -> Vec<String> {
            zoom(id, false)
                .unwrap()
                .lines()
                .map(|l| l.chars().take_while(|&c| c != ' ').collect())
                .collect()
        };
        assert_eq!(zoom("0+2", false).unwrap(), "0|one\n1|two");
        assert_eq!(
            zoom("0+4", false).unwrap(),
            "0|one\n1|two\n2.0|user: m4\n2.1+3|ran\n2.4|(not summarised yet)\n\
             3|(not summarised yet)",
            "four turn lines; turn 2 is not built, so what is built under it"
        );
        assert_eq!(
            heads("2"),
            ["2.0|user:", "2.1+3|ran", "2.4|ai:"],
            "a turn: its message, chunk lines, items"
        );
        assert_eq!(
            zoom("2.1+3", false).unwrap(),
            format!(
                "2.1|ai: {}\n2.2|tool Bash: m6\n2.3|output: (1 line, {} bytes)",
                text(5),
                text(7).len()
            ),
            "a chunk: its items, the output as its size"
        );
        assert!(
            zoom("2.1+3", true)
                .unwrap()
                .ends_with(&format!("2.3|output: {}", text(7)))
        );
        assert_eq!(
            zoom("2.3", false).unwrap(),
            format!("2.3|output: (1 line, {} bytes)", text(7).len())
        );
        assert_eq!(
            zoom("2.3", true).unwrap(),
            format!("2.3|output: {}", text(7))
        );
        assert_eq!(
            heads("1+3"),
            ["1|two", "2.0|user:", "2.1+3|ran", "2.4|(not", "3|(not"]
        );
        assert_eq!(heads("4"), ["4.0|user:", "4.1|ai:"], "the open turn");
        for bad in ["5", "4+2", "0+8", "2.5", "2.3+3", "9.0"] {
            assert_eq!(zoom(bad, false), None, "{bad}");
        }

        let lines = [(2, 3, "a"), (2, 7, "b"), (2, 11, "c"), (2, 15, "d")];
        let b = branch(chain(&"ut".repeat(9), short), &lines);
        assert_eq!(
            b.zoom(Id::parse("0+8").unwrap(), false).unwrap(),
            "0+2|a\n2+2|b\n4+2|c\n6+2|d",
            "two levels down"
        );
    }

    #[test]
    fn grep_finds_words_calls_and_lines_and_output_on_request() {
        let text = |i: usize| {
            [
                "find the needle",
                "looking",
                "grep needle",
                "needle found here",
                "done",
                "thanks",
                "ok",
                "more needle",
                "x",
            ][i]
                .to_string()
        };
        let b = branch(
            chain("utcrtutut", text),
            &[(1, 4, "user asked for the needle"), (2, 6, "needle twice")],
        );
        let re = Regex::new("needle").unwrap();
        let grep = |out: bool, before: Option<&str>| -> Vec<String> {
            let before = before.map(|b| Id::parse(b).unwrap());
            b.grep(&re, out, before)
                .unwrap()
                .into_iter()
                .map(|h| h.line)
                .collect()
        };
        assert_eq!(
            grep(false, None),
            [
                "0.0|user: find the needle",
                "0.2|tool Bash: grep needle",
                "0|user asked for the needle",
                "0+2|needle twice",
                "2.0|user: more needle",
            ]
        );
        assert!(grep(true, None).contains(&"0.3|output: needle found here".to_string()));
        assert_eq!(
            grep(false, Some("0")),
            ["0.0|user: find the needle", "0.2|tool Bash: grep needle"],
            "before turn 0's line: what ends before it"
        );
        assert_eq!(b.grep(&re, false, Some(Id::parse("9").unwrap())), None);
    }
}
