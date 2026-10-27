use std::{collections::HashMap, ops::Range};

use anyhow::{Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::{
    agent::{
        id::Id,
        model::{Item, Tree, Turn},
        record::{Binding, Key},
        render::{Cut, WHOLE, item_line, item_text, searched},
        rollup::{self, chunk_last},
        store::Snapshot,
    },
    config::positive,
    memory::{Coord, excerpt, flat},
};

/// The most of a message's words the fold prints for a turn not summarised yet.
pub const LINE: usize = 512;
/// The most of a tool call's input a zoom lists, on one line.
const CALL: usize = 400;

/// How the fold's fallbacks print an item.
const SHORT: Cut = Cut {
    words: LINE,
    input: LINE,
    output: Some(LINE),
};
/// How a zoom lists an item: its words whole, a tool call on one line, a tool output as its
/// size.
const LISTING: Cut = Cut {
    words: WHOLE,
    input: CALL,
    output: None,
};

/// `[agent]` in `ai.toml`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Table {
    pub view: View,
}

impl Table {
    /// These settings, then `base`'s for those unset here.
    pub fn over(self, base: Table) -> Table {
        Table {
            view: self.view.over(base.view),
        }
    }
}

/// `[agent.view]` in `ai.toml`: how many entries the view shows at each level, as [`Counts`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    #[serde(deserialize_with = "positive")]
    pub total: Option<usize>,
    #[serde(deserialize_with = "positive")]
    pub latest: Option<usize>,
    pub recent: Option<usize>,
    pub single: Option<usize>,
}

impl View {
    /// The counts, defaults for those unset; an error naming the keys when `latest`, `recent`
    /// and `single` add up to more than `total`.
    pub fn counts(&self) -> Result<Counts> {
        let d = Counts::default();
        let c = Counts {
            total: self.total.unwrap_or(d.total),
            latest: self.latest.unwrap_or(d.latest),
            recent: self.recent.unwrap_or(d.recent),
            single: self.single.unwrap_or(d.single),
        };
        let sum = c.latest + c.recent + c.single;
        if sum > c.total {
            bail!(
                "[agent.view]: latest + recent + single ({} + {} + {} = {sum}) is more than total \
                 ({}).",
                c.latest,
                c.recent,
                c.single,
                c.total
            );
        }
        Ok(c)
    }

    /// These settings, then `base`'s for those unset here.
    pub fn over(self, base: View) -> View {
        View {
            total: self.total.or(base.total),
            latest: self.latest.or(base.latest),
            recent: self.recent.or(base.recent),
            single: self.single.or(base.single),
        }
    }
}

/// How many entries the view shows at each level. Each entry counts one: a turn shown item by
/// item, a turn line, or a line over several turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    /// Entries in the view.
    pub total: usize,
    /// The newest turns, every item.
    pub latest: usize,
    /// The turns before them, message and final reply word for word.
    pub recent: usize,
    /// The fewest of the fold's newest turns kept on a line each.
    pub single: usize,
}

impl Default for Counts {
    fn default() -> Counts {
        Counts {
            total: 64,
            latest: 1,
            recent: 3,
            single: 0,
        }
    }
}

impl Counts {
    /// The entries left to the fold: `total` less the turns shown item by item.
    fn fold(&self) -> usize {
        self.total.saturating_sub(self.latest + self.recent)
    }
}

const TURNS: &str = "Turns";

/// The view's opening: what the thread is and how to read it; `{name}` is the thread.
const INSTRUCTION: &str = include_str!("prompts/view.txt");

/// How much of a turn the view shows, least first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    /// A line over several turns.
    Oldest,
    /// Its turn line.
    Older,
    /// Its message and final reply word for word, its work lines between them.
    Recent,
    /// Its message and every item, as a zoom lists them.
    Newest,
}

/// The fold over `t` turns: from one line each, merges the most due aligned pair,
/// due = (t - last)/2^l, oldest first on a tie, until at most `budget` lines are left or no pair
/// is; the newest `keep` turns never merge.
pub fn fold(t: u64, budget: usize, keep: u64) -> Vec<Coord> {
    let mut view: Vec<Coord> = (0..t).map(Coord::leaf).collect();
    let open = t.saturating_sub(keep);
    while view.len() > budget {
        let mut best: Option<(usize, f64)> = None;
        for (k, w) in view.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            if a.l != b.l || a.i % 2 != 0 || b.i != a.i + 1 || b.end() > open {
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
    /// How many entries its view shows at each level.
    counts: Counts,
    /// Read for the session attached to it.
    attached: bool,
}

impl Branch {
    /// The branch ending at item `tip`, none for no item, with `lines` by key, its view by
    /// `counts`.
    pub fn new(
        tree: Tree,
        tip: Option<u64>,
        lines: HashMap<Key, String>,
        counts: Counts,
    ) -> Branch {
        let b = tip.map(|t| rollup::branch(&tree, t as usize));
        let (path, parts) = b.map_or_else(Default::default, |b| (b.seqs, b.parts));
        let (xs, cuts) = parts.into_iter().map(|p| (p.turn, p.chunks)).unzip();
        Branch {
            tree,
            path,
            xs,
            cuts,
            lines,
            counts,
            attached: false,
        }
    }

    /// The branch of `s` ending at item `tip`, with every line of the thread, its view by
    /// `counts`.
    pub fn load(s: &Snapshot, tip: Option<u64>, counts: Counts) -> Result<Branch> {
        let lines = s
            .all_lines()?
            .into_iter()
            .map(|(k, l)| (k, l.text))
            .collect();
        Ok(Branch::new(s.tree()?, tip, lines, counts))
    }

    /// The branch of session `b`: to its tip, else to its graft, the thread's newest item when
    /// it was attached.
    pub fn session(s: &Snapshot, b: &Binding, counts: Counts) -> Result<Branch> {
        let mut branch = Branch::load(s, b.tip.or(b.graft), counts)?;
        branch.attached = true;
        Ok(branch)
    }

    /// The view, in the pieces a page may end between: the instruction, then every turn once,
    /// oldest first, in the entries of [`Branch::levels`], each whole.
    pub fn entries(&self, thread: &str) -> Vec<String> {
        let mut head = format!(
            "# Thread `{thread}`\n\n{}",
            INSTRUCTION.replace("{name}", thread)
        );
        if self.xs.is_empty() {
            head += "\nThe thread holds nothing yet.\n";
        }
        if let Some(which) = self.which() {
            head += &format!("\n{which}\n");
        }
        let list = self.list();
        if !list.is_empty() {
            head += &format!("\n## {TURNS}\n");
        }
        std::iter::once(head)
            .chain(list.into_iter().map(|e| e.join("\n") + "\n"))
            .collect()
    }

    /// Each entry of the view with its level, oldest first: the fold over the turns before the
    /// recent ones in its share of the entries, the recent turns, then the newest turns.
    fn levels(&self) -> Vec<(Coord, Level)> {
        let n = self.xs.len();
        let latest = n.saturating_sub(self.counts.latest);
        let recent = latest.saturating_sub(self.counts.recent);
        let fold = fold(recent as u64, self.counts.fold(), self.counts.single as u64)
            .into_iter()
            .map(|c| match c.l {
                0 => (c, Level::Older),
                _ => (c, Level::Oldest),
            });
        let turns = (recent..n).map(|e| match e < latest {
            true => (Coord::leaf(e as u64), Level::Recent),
            false => (Coord::leaf(e as u64), Level::Newest),
        });
        fold.chain(turns).collect()
    }

    /// The view's entries, each as its lines: the fold's, then each recent and newest turn's
    /// under its number.
    fn list(&self) -> Vec<Vec<String>> {
        let levels = self.levels();
        let fold: Vec<Coord> = levels
            .iter()
            .filter(|(_, l)| *l <= Level::Older)
            .map(|&(c, _)| c)
            .collect();
        let mut out = self.render(&fold);
        for &(c, level) in &levels {
            let e = c.i as usize;
            match level {
                Level::Oldest | Level::Older => {}
                Level::Recent => out.push(headed(e, self.recent_turn(e))),
                Level::Newest => out.push(headed(e, self.newest(e))),
            }
        }
        out
    }

    /// Which of the thread's branches this is, when it has more than one: whose, and the item it
    /// ends at.
    fn which(&self) -> Option<String> {
        let tips = self.tree.tips();
        let &tip = self.path.last()?;
        if tips.len() < 2 {
            return None;
        }
        let whose = match (self.attached, tips.last() == Some(&tip)) {
            (true, _) => "your session's",
            (false, true) => "the newest",
            (false, false) => "an older one",
        };
        let item = &self.tree.items()[tip];
        Some(format!(
            "The thread has {} branches. This view follows {whose}, which ends at {}, written by {} \
             session {} at {}.",
            tips.len(),
            Id::item(self.tree.place(tip)),
            item.id.harness,
            item.id.session,
            item.ts.strftime("%Y-%m-%d %H:%M"),
        ))
    }

    /// The keys of the lines the view wants and lacks: the fold's, and the recent turns' work
    /// lines.
    pub fn needed(&self) -> Vec<Key> {
        let mut keys = Vec::new();
        for (c, level) in self.levels() {
            let e = c.i as usize;
            match level {
                Level::Oldest | Level::Older => keys.push(self.key(c)),
                Level::Recent => keys.extend(self.cuts[e].iter().map(|r| self.chunk_key(e, r))),
                Level::Newest => {}
            }
        }
        keys.retain(|k| !self.lines.contains_key(k));
        keys
    }

    /// What `id` holds, one or two levels down: a node into the lines two levels under it
    /// (`t+2` into its two turn lines), a turn into its chunk lines and items, items into the
    /// chunk lines inside them and their other items, one item whole; turns that are no node
    /// into the fewest nodes that tile them. A line not built shows what is built under it.
    /// Tool output only with `tool_output`. `None` if `id` is not on this branch.
    pub fn zoom(&self, id: Id, tool_output: bool) -> Option<String> {
        let listing = match tool_output {
            true => Cut::WHOLE,
            false => LISTING,
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
                    self.push(c, false, &mut out);
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
    pub fn grep(&self, re: &Regex, tool_output: bool) -> Vec<Hit> {
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
            .collect();
        hits.sort_by_key(|h| h.order);
        hits
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
                (!open).then(|| self.chunk_key(turn as usize, &r))
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

    /// How many turns are closed: all but an open newest one.
    fn closed(&self) -> u64 {
        self.xs.iter().take_while(|x| x.closed).count() as u64
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
            last: self.path[self.xs[c.end() as usize - 1].last] as u64,
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

    /// The key of the line over chunk `r` of turn `e`.
    fn chunk_key(&self, e: usize, r: &Range<usize>) -> Key {
        Key {
            level: 0,
            last: self.path[chunk_last(&self.xs[e], r)] as u64,
        }
    }

    /// The chunk lines built in turn `e`, consecutive from its first step, each with the
    /// positions it covers.
    fn chunks(&self, e: usize) -> Vec<(Range<usize>, &str)> {
        self.cuts[e]
            .iter()
            .map_while(|r| Some((r.clone(), self.lines.get(&self.chunk_key(e, r))?.as_str())))
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

    /// The placeholder of line `c`; with `messages`, a turn's carries its message cut to
    /// [`LINE`].
    fn missing(&self, c: Coord, messages: bool) -> String {
        let id = Id::node(c);
        let first = self.item(self.span(c).start);
        match messages && c.l == 0 && first.kind.opens() {
            true => format!("{id}|(not summarised yet) {}", item_text(first, SHORT)),
            false => Branch::placeholder(id),
        }
    }

    /// The lines of each entry of fold `plan`: its built line, else what is built under it,
    /// else its placeholder with a turn's message; the oldest of the fallbacks become
    /// placeholders while over the fold's share of the entries.
    fn render(&self, plan: &[Coord]) -> Vec<Vec<String>> {
        let mut parts: Vec<Vec<String>> = plan
            .iter()
            .map(|&c| {
                let mut out = Vec::new();
                self.push(c, true, &mut out);
                out
            })
            .collect();
        let mut n: usize = parts.iter().map(Vec::len).sum();
        for (part, &c) in parts.iter_mut().zip(plan) {
            if n <= self.counts.fold() {
                break;
            }
            if part.len() > 1 {
                n = n + 1 - part.len();
                *part = vec![self.missing(c, true)];
            }
        }
        parts
    }

    /// Line `c`, else the lines built under it, else its placeholder: with `messages`, a turn's
    /// carries its message.
    fn push(&self, c: Coord, messages: bool, out: &mut Vec<String>) {
        match self.text(c) {
            Some(t) => out.push(Branch::line(Id::node(c), t)),
            None if !self.built_under(c) => out.push(self.missing(c, messages)),
            None if c.l > 0 => {
                for k in c.children() {
                    self.push(k, messages, out);
                }
            }
            None => {
                let e = c.i as usize;
                let span = self.span(c);
                out.extend((span.start..self.body(e)).map(|p| self.item_line(p, SHORT)));
                self.work(e, span.end, out);
            }
        }
    }

    /// Turn `e`'s chunk lines built consecutive from its first, then a placeholder for the
    /// rest of its items before position `end`.
    fn work(&self, e: usize, end: usize, out: &mut Vec<String>) {
        let chunks = self.chunks(e);
        out.extend(
            chunks
                .iter()
                .map(|(r, t)| Branch::line(self.items_id(r), t)),
        );
        let rest = chunks.last().map_or(self.body(e), |c| c.0.end)..end;
        if !rest.is_empty() {
            out.push(Branch::placeholder(self.items_id(&rest)));
        }
    }

    /// Recent turn `e`: its message and final reply word for word, its work lines between
    /// them.
    fn recent_turn(&self, e: usize) -> Vec<String> {
        let x = &self.xs[e];
        let body = self.body(e);
        let mut out: Vec<String> = (x.span.start..body)
            .map(|p| self.item_line(p, Cut::WHOLE))
            .collect();
        self.work(e, x.work().last().map_or(body, |s| s.end), &mut out);
        out.extend(x.reply.map(|p| self.item_line(p, Cut::WHOLE)));
        out
    }

    /// Newest turn `e`: its message and every item, as a zoom lists them.
    fn newest(&self, e: usize) -> Vec<String> {
        self.xs[e]
            .span
            .clone()
            .map(|p| self.item_line(p, LISTING))
            .collect()
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

/// Turn `e`'s lines under its number on a line of its own, their ids without the turn.
fn headed(e: usize, lines: Vec<String>) -> Vec<String> {
    let turn = format!("{e}.");
    std::iter::once(e.to_string())
        .chain(lines.into_iter().map(|l| match l.strip_prefix(&turn) {
            Some(rest) => format!(".{rest}"),
            None => l,
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use serde_json::Value;

    use super::*;
    use crate::agent::model::{Kind, Marker, SourceId};

    fn source(entry: usize) -> SourceId {
        SourceId {
            harness: "test".into(),
            session: "s".into(),
            entry: entry.to_string(),
        }
    }

    /// Items in one chain, from kinds: `u` user, `w` work, `t` text, `e` text that ends the
    /// turn, `c` call, `r` result, each saying `text`.
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
                    't' | 'e' => Kind::Text(text(i)),
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
                ends_turn: k == 'e',
            })
            .collect()
    }

    fn branch(items: Vec<Item>, lines: &[(u32, u64, &str)]) -> Branch {
        counted(items, lines, Counts::default())
    }

    fn counted(items: Vec<Item>, lines: &[(u32, u64, &str)], counts: Counts) -> Branch {
        let tip = items.len().checked_sub(1).map(|t| t as u64);
        let lines = lines
            .iter()
            .map(|&(level, last, t)| (Key { level, last }, t.to_string()))
            .collect();
        Branch::new(Tree::new(items), tip, lines, counts)
    }

    /// The characters `lines` take, one newline each.
    fn chars(lines: &[String]) -> usize {
        lines.iter().map(|l| l.chars().count() + 1).sum()
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
        assert_eq!(names(fold(10, 6, 0)), ["0+4", "4+2", "6", "7", "8", "9"]);
        assert_eq!(
            names(fold(10, 3, 0)),
            ["0+4", "4+4", "8+2"],
            "due counts from a pair's last turn: 8-9 merges before 0-7"
        );
        assert_eq!(names(fold(10, 10, 0)).len(), 10);
        assert_eq!(
            names(fold(3, 0, 0)),
            ["0+2", "2"],
            "stops when no pair is left"
        );
        assert!(fold(0, 0, 0).is_empty());
        assert_eq!(
            names(fold(10, 3, 2)),
            ["0+8", "8", "9"],
            "the newest two never merge"
        );
        assert_eq!(names(fold(10, 2, 2)), ["0+8", "8", "9"]);
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

    /// The fold over every closed turn of `b`, rendered.
    fn folded(b: &Branch) -> Vec<String> {
        b.render(&fold(b.closed(), b.counts.fold(), 0)).concat()
    }

    #[test]
    fn built_lines_fill_the_fold_and_the_rest_fall_back() {
        let b = branch(chain("utututututututut", short), &[]);
        assert_eq!(b.closed(), 7);
        let all = folded(&b);
        assert_eq!(all.len(), 7);
        assert_eq!(all[0], "0|(not summarised yet) user: m0");
        let b = branch(
            chain("utututututututut", short),
            &[(1, 1, "first"), (1, 3, "second"), (1, 7, "fourth")],
        );
        assert_eq!(
            folded(&b),
            [
                "0|first",
                "1|second",
                "2|(not summarised yet) user: m4",
                "3|fourth",
                "4|(not summarised yet) user: m8",
                "5|(not summarised yet) user: m10",
                "6|(not summarised yet) user: m12",
            ],
        );
    }

    #[test]
    fn the_fold_keeps_to_its_lines() {
        // 66 closed turns in 64 lines: the two oldest pairs merge, 0+2 and 2+2.
        let b = counted(
            chain(&"ut".repeat(67), short),
            &[(2, 3, "first two"), (1, 5, "third"), (1, 131, "newest")],
            Counts {
                total: 68,
                ..Counts::default()
            },
        );
        let fold = folded(&b);
        assert_eq!(fold.len(), b.counts.fold());
        assert_eq!(
            fold[..3],
            [
                "0+2|first two",
                "2+2|(not summarised yet)",
                "4|(not summarised yet) user: m8"
            ],
            "2+2 would fall back to two lines, one too many: it stays one; a turn keeps its message"
        );
        assert_eq!(fold.last().unwrap(), "65|newest");
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
            |lines: &[(u32, u64, &str)]| folded(&branch(chain("utcrtcrtcrut", &text), lines));
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
            ["0|(not summarised yet) user: m0"],
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

    /// The spec's rule on `b`: its entries, and the ids of its lines, cover turns 0..N once
    /// each, in order, their detail never decreases toward the newest, and each level holds its
    /// count.
    fn check(b: &Branch) {
        let n = b.xs.len() as u64;
        let levels = b.levels();
        let mut at = 0;
        for (c, _) in &levels {
            assert_eq!(c.id(), at, "{levels:?}");
            at = c.end();
        }
        assert_eq!(at, n, "{levels:?}");
        assert!(levels.windows(2).all(|w| w[0].1 <= w[1].1), "{levels:?}");
        let count = |level| levels.iter().filter(|(_, l)| *l == level).count();
        let latest = b.counts.latest.min(n as usize);
        assert_eq!(count(Level::Newest), latest);
        assert_eq!(
            count(Level::Recent),
            b.counts.recent.min(n as usize - latest)
        );

        let mut last: Option<(Range<u64>, bool)> = None;
        let mut head: Option<String> = None;
        for l in b.list().concat() {
            let id = l.split('|').next().unwrap();
            if !l.contains('|') {
                head = Some(l.clone());
                continue;
            }
            let id = match id.starts_with('.') {
                true => format!("{}{id}", head.as_ref().expect("an item under its turn")),
                false => id.to_string(),
            };
            let (r, items) = match Id::parse(&id).unwrap() {
                Id::Turns { first, n } => (first..first + n, false),
                Id::Items { turn, .. } => (turn..turn + 1, true),
            };
            match &last {
                Some((p, true)) if items && *p == r => continue,
                Some((p, _)) => assert_eq!(p.end, r.start, "{l}"),
                None => assert_eq!(r.start, 0, "{l}"),
            }
            last = Some((r, items));
        }
        assert_eq!(last.map_or(0, |(r, _)| r.end), n);
    }

    #[test]
    fn every_turn_shows_once_in_order_and_detail_never_decreases() {
        let big = padded(|i| if i % 2 == 0 { 2_000 } else { 0 });
        let lines = turn_lines(200, "said");
        let mut nodes = as_refs(&lines);
        nodes.extend([(2, 3, "0 and 1"), (3, 7, "0 to 3"), (2, 11, "4 and 5")]);
        let work = padded(|i| if i % 5 == 0 { 3_000 } else { 0 });
        let huge = padded(|i| if i == 0 { 20_000 } else { 0 });
        for b in [
            branch(Vec::new(), &[]),
            branch(chain("u", short), &[]),
            branch(chain("uct", short), &[]),
            branch(chain(&("ue".repeat(10) + "u"), short), &[]),
            branch(chain(&"ue".repeat(200), &big), &[]),
            branch(chain(&"ue".repeat(200), &big), &as_refs(&lines)),
            branch(chain(&"ue".repeat(200), &big), &nodes),
            branch(
                chain(&"ucrte".repeat(40), &work),
                &[(0, 8, "w"), (0, 193, "w")],
            ),
            branch(chain("ueu", &huge), &[]),
            branch(chain(&"ut".repeat(67), short), &[(2, 3, "first two")]),
            counted(
                chain(&"ue".repeat(40), short),
                &[],
                Counts {
                    total: 12,
                    latest: 2,
                    recent: 4,
                    single: 5,
                },
            ),
        ] {
            check(&b);
        }
    }

    #[test]
    fn each_level_holds_its_count_back_from_the_newest() {
        let counts = Counts {
            total: 10,
            latest: 2,
            recent: 3,
            single: 3,
        };
        let levels = |turns: usize| -> Vec<(String, Level)> {
            counted(chain(&"ue".repeat(turns), short), &[], counts)
                .levels()
                .into_iter()
                .map(|(c, l)| (c.to_string(), l))
                .collect()
        };
        let names = |v: &[(String, Level)]| v.iter().map(|(c, _)| c.clone()).collect::<Vec<_>>();
        let all = levels(32);
        assert_eq!(all.len(), counts.total);
        assert_eq!(
            names(&all),
            [
                "0+16", "16+8", "24", "25", "26", "27", "28", "29", "30", "31"
            ]
            .map(String::from),
            "{all:?}"
        );
        let at = |v: &[(String, Level)], c: &str| v.iter().find(|(n, _)| n == c).unwrap().1;
        assert_eq!(
            at(&all, "26"),
            Level::Older,
            "the fold keeps its newest three single"
        );
        assert_eq!(at(&all, "27"), Level::Recent);
        assert_eq!(at(&all, "29"), Level::Recent);
        assert_eq!(at(&all, "30"), Level::Newest);
        assert_eq!(at(&all, "31"), Level::Newest);

        let few = levels(4);
        assert_eq!(
            few.iter().map(|(_, l)| *l).collect::<Vec<_>>(),
            [Level::Recent, Level::Recent, Level::Newest, Level::Newest],
            "a short thread fills the newest levels first"
        );
        assert!(levels(0).is_empty());
    }

    #[test]
    fn counts_over_the_total_are_an_error_naming_the_keys() {
        let view = View {
            total: Some(5),
            single: Some(2),
            ..View::default()
        };
        let err = view.counts().unwrap_err().to_string();
        assert_eq!(
            err,
            "[agent.view]: latest + recent + single (1 + 3 + 2 = 6) is more than total (5)."
        );
        assert_eq!(View::default().counts().unwrap(), Counts::default());
    }

    #[test]
    fn a_recent_turn_shows_its_message_work_lines_and_final_reply() {
        // Turn 0: message 0, work 1-5, final reply 6; turn 1 is recent too, turn 2 the newest.
        let text = padded(|i| if i == 0 { 9_000 } else { 0 });
        let spec = "ucrtcreueu";
        let b = branch(chain(spec, &text), &[(0, 5, "ran it")]);
        let turn = b.recent_turn(0);
        assert_eq!(ids(&turn), ["0.0", "0.1+5", "0.6"]);
        assert_eq!(turn[0], format!("0.0|user: {}", text(0)), "word for word");
        assert_eq!(turn[1], "0.1+5|ran it");
        assert_eq!(turn[2], "0.6|ai: m6");
        assert_eq!(
            section_lines(&b.entries("dev").concat(), TURNS)[..4],
            [
                "0",
                &format!(".0|user: {}", text(0)),
                ".1+5|ran it",
                ".6|ai: m6"
            ],
            "under its number"
        );

        let b = branch(chain(spec, &text), &[]);
        assert_eq!(b.recent_turn(0)[1], "0.1+5|(not summarised yet)");

        assert_eq!(
            branch(chain("uctuct", short), &[]).recent_turn(0),
            ["0.0|user: m0", "0.1+2|(not summarised yet)"],
            "a turn cut short by a message has no final reply: all of it is work"
        );
        assert_eq!(
            branch(chain("ueu", short), &[]).recent_turn(0),
            ["0.0|user: m0", "0.1|ai: m1"]
        );
    }

    #[test]
    fn the_newest_turn_lists_every_item_as_zoom_does() {
        let text = |i: usize| match i {
            3 => "o".repeat(5_000),
            _ => format!("say {i} {}", "w".repeat(9_000)),
        };
        let b = branch(chain("ueucrte", text), &[]);
        let newest = b.newest(1);
        assert_eq!(ids(&newest), ["1.0", "1.1", "1.2", "1.3", "1.4"]);
        assert_eq!(
            newest.join("\n"),
            b.zoom(Id::parse("1").unwrap(), false).unwrap()
        );
        assert!(newest[0].ends_with(&text(2)) && newest[4].ends_with(&text(6)));
        assert!(newest[1].len() < 500 && newest[2].starts_with("1.2|output: (1 line, "));
        let b = branch(chain("ueucrte", text), &[(0, 5, "did it")]);
        assert_eq!(
            ids(&b.newest(1)),
            ["1.0", "1.1", "1.2", "1.3", "1.4"],
            "a work line built changes nothing"
        );
    }

    #[test]
    fn the_view_needs_the_fold_and_recent_work_lines_whatever_is_built() {
        // 30 turns of 5 items: 0-25 in the fold, one line each; 26-28 recent; 29 the newest.
        let items = chain(&"ucrte".repeat(30), short);
        let b = branch(items.clone(), &[]);
        let key = |level, last| Key { level, last };
        let want: Vec<Key> = (0..26)
            .map(|e| key(1, 5 * e + 4))
            .chain((26..29).map(|e| key(0, 5 * e + 3)))
            .collect();
        assert_eq!(b.needed(), want, "the newest turn needs no line");

        let built = want.iter().map(|&k| (k, "w".repeat(2_000))).collect();
        let tip = Some(items.len() as u64 - 1);
        let built = Branch::new(Tree::new(items), tip, built, Counts::default());
        assert_eq!(
            built.levels(),
            b.levels(),
            "what is built never moves a turn between levels"
        );
        assert!(built.needed().is_empty());
        check(&built);
    }

    #[test]
    fn the_view_is_one_list_of_bounded_size() {
        let spec = "ue".repeat(3000) + "u";
        let lines = turn_lines(3000, &"y".repeat(500));
        let big = |i: usize| format!("{i} {}", "z".repeat(20_000));
        for b in [
            branch(chain(&spec, short), &as_refs(&lines)),
            branch(chain(&spec, big), &as_refs(&lines)),
            branch(chain(&spec, big), &[]),
        ] {
            let v = b.entries("dev").concat();
            let recent: usize = (2997..3000).map(|e| chars(&b.recent_turn(e))).sum();
            let most = b.counts.fold() * 600 + recent + chars(&b.newest(3000)) + 4_000;
            assert!(v.chars().count() <= most, "{}", v.chars().count());
            assert!(v.starts_with("# Thread `dev`\n\nThis is the thread `dev`"));
            assert_eq!(v.matches("\n## ").count(), 1, "one list");
            let fold = b
                .levels()
                .iter()
                .filter(|(_, l)| *l <= Level::Older)
                .count();
            assert_eq!(fold, b.counts.fold());
            let list = section_lines(&v, TURNS);
            assert_eq!(list[list.len() - 2], "3000");
            assert!(list.last().unwrap().starts_with(".0|user: "));
            check(&b);
        }
        let empty = branch(Vec::new(), &[]).entries("dev").concat();
        assert!(empty.ends_with("The thread holds nothing yet.\n"));
    }

    #[test]
    fn each_entry_is_a_whole_turn_or_fold_line() {
        let b = branch(chain(&"ucrte".repeat(30), short), &[]);
        let entries = b.entries("dev");
        assert!(entries[0].starts_with("# Thread `dev`\n"));
        assert!(
            entries[0].ends_with("\n## Turns\n"),
            "the instruction is one entry"
        );
        let turns = &entries[1..];
        assert_eq!(turns.len(), 26 + 3 + 1);
        for (e, t) in (26..30).zip(&turns[26..]) {
            assert!(t.starts_with(&format!("{e}\n.0|user: ")), "{t}");
            assert_eq!(t.matches("\n.4|").count(), 1, "turn {e} whole: {t}");
        }
        assert!(turns[..26].iter().all(|t| !t.starts_with('.')));
    }

    #[test]
    fn the_view_names_its_branch_among_several() {
        let mut items = chain("ututu", short);
        items[4].parent = Some(source(1));
        let view = |tip: u64, attached: bool| {
            let mut b = Branch::new(
                Tree::new(items.clone()),
                Some(tip),
                HashMap::new(),
                Counts::default(),
            );
            b.attached = attached;
            b.entries("dev").concat()
        };
        let at = "written by test session s at 1970-01-01 00:00.";
        assert!(view(4, false).contains(&format!(
            "\nThe thread has 2 branches. This view follows the newest, which ends at 1.0, {at}\n"
        )));
        assert!(view(3, false).contains("follows an older one, which ends at 1.1, "));
        assert!(view(3, true).contains("follows your session's, which ends at 1.1, "));
        assert!(
            !branch(chain("ututu", short), &[])
                .entries("dev")
                .concat()
                .contains("branches.")
        );
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
        let grep =
            |out: bool| -> Vec<String> { b.grep(&re, out).into_iter().map(|h| h.line).collect() };
        assert_eq!(
            grep(false),
            [
                "0.0|user: find the needle",
                "0.2|tool Bash: grep needle",
                "0|user asked for the needle",
                "0+2|needle twice",
                "2.0|user: more needle",
            ]
        );
        assert!(grep(true).contains(&"0.3|output: needle found here".to_string()));
    }
}
