use std::{collections::BTreeMap, ops::Range};

use crate::{
    chat::{
        id::Id,
        model::{Item, Kind, Place, Tree, Turn, turns},
        record::{Key, LineKind},
        render::{Cut, clip, item_line},
    },
    memory::{WORDS, flat, limit, ruler},
};

/// The system prompt of every roll-up call.
pub const PROMPT: &str = include_str!("prompts/compact.txt");
/// The most words a line is asked for, however much it covers.
pub const MAX_WORDS: usize = 200;
/// The words of the AI's own a line covers per word it is asked for.
const RATIO: usize = 10;
/// What a tool call adds to what a line covers, in words of the AI's own.
const CALL: usize = 20;
/// The most one call reads of a turn's work, in bytes; longer work is cut into chunks.
pub const INPUT: usize = 48 * 1024;
/// The most a call reads of one tool call's input or one tool output: its head and tail.
pub const CLIP: usize = 4 * 1024;
/// How a call reads an item: a message or reply in at most half the input, so no item alone
/// fills a chunk; tool inputs and outputs in [`CLIP`].
const CUT: Cut = Cut {
    words: INPUT / 2,
    input: CLIP,
    output: Some(CLIP),
};

/// The item at `place` as a call reads it: its line, cut by [`CUT`], and a newline.
pub fn render(place: Place, item: &Item) -> String {
    item_line(Id::item(place), item, CUT) + "\n"
}

/// What an item adds to what a line covers: the AI's own words, and [`CALL`] for a tool call.
/// Messages and tool output add nothing.
fn weight(item: &Item) -> usize {
    match &item.kind {
        Kind::Text(t) => t.split_whitespace().count(),
        Kind::ToolCall { .. } => CALL,
        _ => 0,
    }
}

/// An item as a call reads it, without its id or the indent of its continued lines.
fn bare(rendered: &str) -> String {
    let text = rendered.split_once('|').map_or(rendered, |(_, t)| t);
    text.replace("\n  ", "\n")
}

/// Cuts `steps`, consecutive ranges of positions, into chunks of at most `budget` bytes,
/// greedily from the first: whole steps while they fit. A step over the budget starts a chunk
/// and is cut the same way before its tool calls; a piece still over it is a chunk alone.
/// `size` gives a position's bytes, `call` whether it is a tool call.
pub fn chunk(
    steps: &[Range<usize>],
    budget: usize,
    size: impl Fn(usize) -> usize,
    call: impl Fn(usize) -> bool,
) -> Vec<Range<usize>> {
    let bytes = |r: &Range<usize>| r.clone().map(&size).sum::<usize>();
    let mut p = Pack {
        budget,
        out: Vec::new(),
        open: None,
    };
    for s in steps {
        let n = bytes(s);
        if n <= budget {
            p.add(s.clone(), n);
            continue;
        }
        p.cut();
        let mut start = s.start;
        for i in s.start + 1..=s.end {
            if i == s.end || call(i) {
                p.add(start..i, bytes(&(start..i)));
                start = i;
            }
        }
    }
    p.cut();
    p.out
}

/// Consecutive pieces packed into chunks of at most `budget` bytes, a piece over it alone.
struct Pack {
    budget: usize,
    out: Vec<Range<usize>>,
    open: Option<(Range<usize>, usize)>,
}

impl Pack {
    fn add(&mut self, piece: Range<usize>, n: usize) {
        if let Some((r, m)) = &mut self.open
            && *m + n <= self.budget
        {
            r.end = piece.end;
            *m += n;
            return;
        }
        self.cut();
        self.open = Some((piece, n));
    }

    fn cut(&mut self) {
        self.out.extend(self.open.take().map(|(r, _)| r));
    }
}

/// What a line is made from. Items are sequence numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A turn's work, or one chunk of it when the work is over [`INPUT`] (`whole` false), read
    /// with the message it answers.
    Chunk {
        message: Option<u64>,
        items: Vec<u64>,
        whole: bool,
    },
    /// A turn: its message, its work's chunk lines in order, and its final reply.
    Turn {
        message: Option<u64>,
        work: Vec<Key>,
        reply: Option<u64>,
    },
    /// Two adjacent lines, the earlier first.
    Node([Key; 2]),
}

impl Source {
    pub fn kind(&self) -> LineKind {
        match self {
            Source::Chunk { .. } => LineKind::Chunk,
            Source::Turn { .. } => LineKind::Turn,
            Source::Node(_) => LineKind::Node,
        }
    }

    /// The lines it is made from.
    pub fn children(&self) -> &[Key] {
        match self {
            Source::Turn { work, .. } => work,
            Source::Node(pair) => pair,
            Source::Chunk { .. } => &[],
        }
    }
}

/// One turn of a branch, with the chunks of its work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub turn: Turn,
    /// Positions on the path: one chunk when its work fits in [`INPUT`], none without work.
    pub chunks: Vec<Range<usize>>,
}

impl Part {
    /// The chunks that no longer change, so have lines: all once the turn is closed, else
    /// all but the last, which may still grow.
    pub fn done(&self) -> &[Range<usize>] {
        let open = !self.turn.closed && !self.chunks.is_empty();
        &self.chunks[..self.chunks.len() - usize::from(open)]
    }
}

/// The turns of one branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// The sequence number of each position on the path, root first.
    pub seqs: Vec<usize>,
    pub parts: Vec<Part>,
}

impl Branch {
    /// The key of the line of `level` that ends at position `at`.
    pub fn key(&self, level: u32, at: usize) -> Key {
        Key {
            level,
            last: self.seqs[at] as u64,
        }
    }

    /// The chunk lines of `p`: of its [`Part::done`] chunks, keyed by their last item, the last
    /// chunk by the turn's [`Turn::last`].
    pub fn chunk_keys(&self, p: &Part) -> Vec<Key> {
        p.done()
            .iter()
            .map(|c| self.key(0, chunk_last(&p.turn, c)))
            .collect()
    }

    /// The turn line of `p`, once it is closed.
    pub fn line(&self, p: &Part) -> Option<Key> {
        let x = &p.turn;
        x.closed.then(|| self.key(1, x.last))
    }

    /// The turn lines of the closed turns, in order.
    pub fn lines(&self) -> Vec<Key> {
        self.parts.iter().filter_map(|p| self.line(p)).collect()
    }
}

/// The position chunk `c` of `turn` is keyed by: its last item, at most the turn's
/// [`Turn::last`].
pub fn chunk_last(turn: &Turn, c: &Range<usize>) -> usize {
    (c.end - 1).min(turn.last)
}

/// The binary nodes over the adjacent lines `lines`, aligned from the first, each with its two
/// halves; lowest level first.
pub fn nodes(lines: &[Key]) -> Vec<(Key, [Key; 2])> {
    let mut out = Vec::new();
    let mut row = lines.to_vec();
    while row.len() > 1 {
        row = row
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[a, b]| {
                let k = Key {
                    level: b.level + 1,
                    last: b.last,
                };
                out.push((k, [a, b]));
                k
            })
            .collect();
    }
    out
}

const WORK: &str =
    "Compress the work of this turn, the steps between its message and its final reply,";
const PART: &str = "Compress this part of the work of a long turn";
const TURN: &str =
    "Compress this turn, given as its message, the lines of its work in order and its final reply,";
const MERGE: &str = "Merge these two adjacent lines, the earlier first,";

/// A call's message: the size of a line of `words` words, what to do, the context and the
/// input.
fn task(what: &str, words: usize, context: Option<&str>, input: &str) -> String {
    let context = context.map_or(String::new(), |c| format!("<context>\n{c}</context>\n"));
    format!(
        "{}{what} into one line of at most {words} words.\n{context}<input>\n{input}</input>",
        ruler(words)
    )
}

/// The turns from the root down to `tip` alone, with their chunks.
pub fn branch(tree: &Tree, tip: usize) -> Branch {
    let seqs = tree.branch(tip);
    let mut size = vec![0; tree.items().len()];
    for &s in &seqs {
        size[s] = render(tree.place(s), &tree.items()[s]).len();
    }
    cut(tree, seqs, |s| size[s])
}

/// The turns of the path `seqs`, with the chunks of their work; `size` gives an item's rendered
/// bytes by sequence number.
fn cut(tree: &Tree, seqs: Vec<usize>, size: impl Fn(usize) -> usize) -> Branch {
    let path: Vec<&Item> = seqs.iter().map(|&i| &tree.items()[i]).collect();
    let size = |p: usize| size(seqs[p]);
    let call = |p: usize| matches!(path[p].kind, Kind::ToolCall { .. });
    let parts = turns(&path)
        .into_iter()
        .map(|x| Part {
            chunks: chunk(x.work(), INPUT, size, call),
            turn: x,
        })
        .collect();
    Branch { seqs, parts }
}

/// The roll-up of a thread's main tree: every line of every branch, with what it is made from.
pub struct Rollup {
    texts: Vec<String>,
    places: Vec<Place>,
    weights: Vec<usize>,
    lines: BTreeMap<Key, Source>,
}

impl Rollup {
    pub fn new(tree: &Tree) -> Rollup {
        let places: Vec<Place> = (0..tree.items().len()).map(|i| tree.place(i)).collect();
        let texts = places
            .iter()
            .zip(tree.items())
            .map(|(&p, item)| render(p, item))
            .collect();
        let mut r = Rollup {
            texts,
            places,
            weights: tree.items().iter().map(weight).collect(),
            lines: BTreeMap::new(),
        };
        for tip in tree.tips() {
            let b = r.branch(tree, tip);
            r.add(tree, &b);
        }
        r
    }

    /// The turns from the root down to `tip`, with their chunks.
    pub fn branch(&self, tree: &Tree, tip: usize) -> Branch {
        cut(tree, tree.branch(tip), |s| self.texts[s].len())
    }

    fn add(&mut self, tree: &Tree, b: &Branch) {
        let seqs = |r: &Range<usize>| r.clone().map(|p| b.seqs[p] as u64).collect::<Vec<_>>();
        for p in &b.parts {
            let first = b.seqs[p.turn.span.start];
            let message = tree.items()[first].kind.opens().then_some(first as u64);
            let keys = b.chunk_keys(p);
            let whole = p.turn.closed && keys.len() == 1;
            for (c, &k) in p.done().iter().zip(&keys) {
                self.lines.entry(k).or_insert_with(|| Source::Chunk {
                    message,
                    items: seqs(c),
                    whole,
                });
            }
            if let Some(k) = b.line(p) {
                self.lines.entry(k).or_insert_with(|| Source::Turn {
                    message,
                    work: keys,
                    reply: p.turn.reply.map(|r| b.seqs[r] as u64),
                });
            }
        }
        for (k, pair) in nodes(&b.lines()) {
            self.lines.entry(k).or_insert(Source::Node(pair));
        }
    }

    /// Every line, by key.
    pub fn lines(&self) -> &BTreeMap<Key, Source> {
        &self.lines
    }

    pub fn source(&self, key: Key) -> Option<&Source> {
        self.lines.get(&key)
    }

    /// The lines made from line `key`, and from those, up to the top.
    pub fn above(&self, key: Key) -> Vec<Key> {
        let mut out = Vec::new();
        let mut stack = vec![key];
        while let Some(k) = stack.pop() {
            for (&p, s) in &self.lines {
                if s.children().contains(&k) && !out.contains(&p) {
                    out.push(p);
                    stack.push(p);
                }
            }
        }
        out
    }

    /// Item `seq` as a call reads it.
    pub fn text(&self, seq: u64) -> &str {
        &self.texts[seq as usize]
    }

    fn place(&self, seq: u64) -> Place {
        self.places[seq as usize]
    }

    /// What line `key` covers, by [`weight`].
    fn size(&self, key: Key) -> usize {
        let weight = |s: &u64| self.weights[*s as usize];
        match self.lines.get(&key) {
            Some(Source::Chunk { items, .. }) => items.iter().map(weight).sum(),
            Some(Source::Turn {
                message,
                work,
                reply,
            }) => {
                let ends: usize = message.iter().chain(reply).map(weight).sum();
                ends + work.iter().map(|&k| self.size(k)).sum::<usize>()
            }
            Some(Source::Node([a, b])) => self.size(*a) + self.size(*b),
            None => 0,
        }
    }

    /// Chunk line `key` as a turn's input lists it: `t.i+n|text`.
    fn chunk_line(&self, key: Key, text: &str) -> String {
        let first = match self.lines.get(&key) {
            Some(Source::Chunk { items, .. }) => items[0],
            _ => key.last,
        };
        let id = Id::line(0, self.place(first), self.place(key.last));
        format!("{id}|{text}\n")
    }

    /// The words line `key` is asked for: one per [`RATIO`] of what it covers, at least
    /// [`WORDS`] and at most [`MAX_WORDS`].
    pub fn words(&self, key: Key) -> usize {
        (self.size(key) / RATIO).clamp(WORDS, MAX_WORDS)
    }

    /// What line `key` is made from, as a call reads it; `None` until every line it is made
    /// from is `built`.
    fn input<'a>(&self, key: Key, built: impl Fn(Key) -> Option<&'a str>) -> Option<String> {
        Some(match self.lines.get(&key)? {
            Source::Chunk { items, .. } => items.iter().map(|&s| self.text(s)).collect(),
            Source::Turn {
                message,
                work,
                reply,
            } => {
                let mut s = message.map_or(String::new(), |m| self.text(m).into());
                for &k in work {
                    s += &self.chunk_line(k, built(k)?);
                }
                s + reply.map_or("", |r| self.text(r))
            }
            Source::Node([a, b]) => format!("{}\n{}\n", built(*a)?, built(*b)?),
        })
    }

    /// The text of line `key` when what it is made from fits its [`limit`], so it needs no
    /// call; `None` when it needs one, or until every line it is made from is `built`.
    pub fn free<'a>(&self, key: Key, built: impl Fn(Key) -> Option<&'a str>) -> Option<String> {
        let max = limit(self.words(key));
        let len = |s: &u64| self.text(*s).len();
        let text = match self.lines.get(&key)? {
            Source::Chunk { items, .. } => {
                if items.iter().map(len).sum::<usize>() > max {
                    return None;
                }
                items.iter().map(|&s| bare(self.text(s))).collect()
            }
            Source::Turn {
                message,
                work,
                reply,
            } => {
                let lines = work.iter().map(|&k| built(k)).collect::<Option<Vec<_>>>()?;
                let ends: usize = message.iter().chain(reply).map(len).sum();
                if ends + lines.iter().map(|l| l.len() + 1).sum::<usize>() > max {
                    return None;
                }
                let mut s = message.map_or(String::new(), |m| bare(self.text(m)));
                for l in lines {
                    s += &format!("{l}\n");
                }
                s + &reply.map_or(String::new(), |r| bare(self.text(r)))
            }
            Source::Node(_) => self.input(key, built)?,
        };
        (text.len() <= max).then(|| flat(text.trim_end()))
    }

    /// The message of the call that writes line `key`; `None` until every line it is made
    /// from is `built`.
    pub fn task<'a>(&self, key: Key, built: impl Fn(Key) -> Option<&'a str>) -> Option<String> {
        let source = self.lines.get(&key)?;
        let input = self.input(key, built)?;
        let words = self.words(key);
        Some(match source {
            Source::Chunk { message, whole, .. } => task(
                if *whole { WORK } else { PART },
                words,
                message.map(|m| clip(self.text(m), CLIP)).as_deref(),
                &input,
            ),
            Source::Turn { .. } => task(TURN, words, None, &input),
            Source::Node(_) => task(MERGE, words, None, &input),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::SourceId;
    use jiff::Timestamp;

    fn item(kind: Kind) -> Item {
        Item {
            id: SourceId {
                harness: "test".into(),
                session: "s".into(),
                entry: "e".into(),
            },
            parent: None,
            ts: Timestamp::UNIX_EPOCH,
            kind,
            ends_turn: false,
        }
    }

    #[test]
    fn a_call_reads_items_cut_to_their_share() {
        let r = |k| render(Place { turn: 2, item: 7 }, &item(k));
        assert_eq!(r(Kind::User("fix it".into())), "2.7|user: fix it\n");
        let result = r(Kind::ToolResult {
            id: "t1".into(),
            output: "o".repeat(3 * CLIP),
            is_error: false,
        });
        assert!(result.len() < CLIP + 100 && result.contains("bytes cut]"));
        let reply = r(Kind::Text("t".repeat(INPUT)));
        assert!(reply.len() < INPUT / 2 + 100 && reply.len() > INPUT / 2);
    }

    /// Chunks of `sizes` (one per position), steps cut where `steps` start, calls at `calls`.
    fn chunks(
        sizes: &[usize],
        steps: &[usize],
        calls: &[usize],
        budget: usize,
    ) -> Vec<(usize, usize)> {
        let ends: Vec<usize> = steps.iter().skip(1).copied().chain([sizes.len()]).collect();
        let steps: Vec<Range<usize>> = steps.iter().zip(&ends).map(|(&a, &b)| a..b).collect();
        chunk(&steps, budget, |i| sizes[i], |i| calls.contains(&i))
            .into_iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    #[test]
    fn chunks_are_whole_steps_while_they_fit() {
        assert_eq!(chunks(&[3, 3, 4], &[0, 1, 2], &[], 10), [(0, 3)]);
        assert_eq!(chunks(&[3, 3, 5], &[0, 1, 2], &[], 10), [(0, 2), (2, 3)]);
        assert_eq!(
            chunks(&[4, 4, 4, 4, 4], &[0, 2, 4], &[], 8),
            [(0, 2), (2, 4), (4, 5)]
        );
        assert_eq!(chunks(&[], &[], &[], 8), []);
    }

    #[test]
    fn a_step_over_the_budget_is_cut_before_its_calls() {
        // step 0: [0]; step 1: text 1, call 2, result 3, call 4, result 5, call 6, result 7
        let sizes = [2, 1, 3, 3, 3, 3, 1, 1];
        assert_eq!(
            chunks(&sizes, &[0, 1], &[2, 4, 6], 8),
            [(0, 1), (1, 4), (4, 8)],
            "the step starts a chunk; pieces pack greedily"
        );
        assert_eq!(
            chunks(&[2, 1, 20, 1, 2], &[0, 1, 4], &[2, 3], 8),
            [(0, 1), (1, 2), (2, 3), (3, 5)],
            "a piece over the budget is a chunk alone; the next step joins the last piece"
        );
    }

    #[test]
    fn chunks_but_the_last_stay_as_items_are_appended() {
        let mut sizes = Vec::new();
        let mut last = Vec::new();
        for n in 0..40 {
            sizes.push(1 + (n * 7) % 5);
            let steps: Vec<usize> = (0..=n).filter(|i| i % 5 == 0).collect();
            let calls: Vec<usize> = (0..=n).filter(|i| i % 5 == 1 || i % 5 == 3).collect();
            let now = chunks(&sizes, &steps, &calls, 8);
            let done = &last[..last.len().saturating_sub(1)];
            assert_eq!(&now[..done.len()], done, "at {n}");
            last = now;
        }
        assert!(last.len() > 5);
    }

    #[test]
    fn nodes_pair_aligned_lines_upwards() {
        let l = |last| Key { level: 1, last };
        let k = |level, last| Key { level, last };
        let n = nodes(&[l(3), l(7), l(9), l(12), l(20)]);
        assert_eq!(
            n,
            [
                (k(2, 7), [l(3), l(7)]),
                (k(2, 12), [l(9), l(12)]),
                (k(3, 12), [k(2, 7), k(2, 12)]),
            ]
        );
        assert!(nodes(&[l(1)]).is_empty());
    }
}
