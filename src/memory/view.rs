use anyhow::Result;

use crate::memory::{store::Snapshot, tree::Coord};

pub const VIEW: u64 = 128_000;
pub const PLACEHOLDER: &str = "(not summarized yet: zoom it)";
/// What zoom shows for a line of tool calls only, which the view leaves out.
pub const HIDDEN: &str = "(tool calls: zoom it)";

/// The tree in memory and the view folded over it: parts tiling `[0, t)`, oldest first.
#[derive(Clone, Debug)]
pub struct Mem {
    t: u64,
    nodes: Vec<Vec<Option<String>>>,
    low: Vec<u64>,
    view: Vec<Coord>,
    size: u64,
    budget: u64,
}

impl Mem {
    pub fn new(budget: u64) -> Mem {
        Mem {
            t: 0,
            nodes: Vec::new(),
            low: Vec::new(),
            view: Vec::new(),
            size: 0,
            budget,
        }
    }

    /// The memory of `s`, its view folded again from message 0.
    pub fn load(s: &Snapshot, budget: u64) -> Result<Mem> {
        let mut m = Mem::new(budget);
        let t = s.log_len()?;
        let mut l = 0;
        while 1u64 << l <= t {
            for (i, n) in s.level(l)?.into_iter().enumerate() {
                if let Some(n) = n {
                    m.set(Coord::new(l, i as u64), n.text);
                }
            }
            l += 1;
        }
        m.t = t;
        m.refold();
        Ok(m)
    }

    pub fn t(&self) -> u64 {
        self.t
    }

    pub fn view(&self) -> &[Coord] {
        &self.view
    }

    /// Bytes of the view's texts, placeholders included.
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    pub fn text(&self, c: Coord) -> Option<&str> {
        self.nodes.get(c.l as usize)?.get(c.i as usize)?.as_deref()
    }

    pub fn built(&self, c: Coord) -> bool {
        self.text(c).is_some()
    }

    /// How many nodes of level `l` are built.
    pub fn built_at(&self, l: u32) -> u64 {
        self.nodes
            .get(l as usize)
            .map_or(0, |n| n.iter().flatten().count() as u64)
    }

    /// Whether `c`'s sources exist: its message, or both children.
    pub fn ready(&self, c: Coord) -> bool {
        match c.l {
            0 => c.i < self.t,
            _ => c.children().iter().all(|&k| self.built(k)),
        }
    }

    fn part_size(&self, c: Coord) -> u64 {
        self.text(c).unwrap_or(PLACEHOLDER).len() as u64
    }

    /// Records node `c` unless it is built; returns whether it was new. Does not refit.
    pub fn set(&mut self, c: Coord, text: String) -> bool {
        if self.built(c) {
            return false;
        }
        let (l, i) = (c.l as usize, c.i as usize);
        if self.nodes.len() <= l {
            self.nodes.resize(l + 1, Vec::new());
            self.low.resize(l + 1, 0);
        }
        if c.l == 0 && c.i < self.t {
            self.size = self.size + text.len() as u64 - PLACEHOLDER.len() as u64;
        }
        let level = &mut self.nodes[l];
        if level.len() <= i {
            level.resize(i + 1, None);
        }
        level[i] = Some(text);
        while level.get(self.low[l] as usize).is_some_and(Option::is_some) {
            self.low[l] += 1;
        }
        true
    }

    pub fn add_message(&mut self) {
        let c = Coord::leaf(self.t);
        self.t += 1;
        self.view.push(c);
        self.size += self.part_size(c);
        self.fit(self.t);
    }

    pub fn add_node(&mut self, c: Coord, text: String) {
        self.set(c, text);
        self.fit(self.t);
    }

    /// While over budget, merges the most due adjacent pair whose parent is built. Never splits.
    pub fn fit(&mut self, t: u64) {
        while self.size > self.budget {
            let mut best: Option<(usize, f64)> = None;
            for (k, w) in self.view.windows(2).enumerate() {
                let (a, b) = (w[0], w[1]);
                if a.l != b.l || a.i % 2 != 0 || b.i != a.i + 1 || !self.built(a.parent()) {
                    continue;
                }
                let due = (t - a.id()) as f64 / (1u64 << (a.l + 2)) as f64;
                if best.is_none_or(|(_, d)| due > d) {
                    best = Some((k, due));
                }
            }
            let Some((k, _)) = best else {
                break;
            };
            let (a, b) = (self.view[k], self.view[k + 1]);
            let p = a.parent();
            self.size = self.size + self.part_size(p) - self.part_size(a) - self.part_size(b);
            self.view.splice(k..k + 2, [p]);
        }
    }

    /// Folds the view again from message 0 over the current tree.
    pub fn refold(&mut self) {
        let t = self.t;
        self.view.clear();
        self.size = 0;
        for i in 0..t {
            let c = Coord::leaf(i);
            self.view.push(c);
            self.size += self.part_size(c);
            self.fit(i + 1);
        }
    }

    pub fn all_built(&self) -> bool {
        self.view.iter().all(|&c| self.built(c))
    }

    /// Nodes to build now, oldest level first: ready, unbuilt, not `busy`. At most `max`.
    pub fn due(&self, busy: impl Fn(Coord) -> bool, max: usize) -> Vec<Coord> {
        let mut out = Vec::new();
        let mut l = 0;
        while 1u64 << l <= self.t {
            let mut i = self.low.get(l as usize).copied().unwrap_or(0);
            loop {
                let c = Coord::new(l, i);
                if c.end() > self.t {
                    break;
                }
                if !self.built(c) && !busy(c) && self.ready(c) {
                    if out.len() >= max {
                        return out;
                    }
                    out.push(c);
                }
                i += 1;
            }
            l += 1;
        }
        out
    }

    /// Takes in the messages `s` holds beyond `t`, with the nodes already built over them.
    pub fn absorb(&mut self, s: &Snapshot) -> Result<()> {
        for i in self.t..s.log_len()? {
            let mut c = Coord::leaf(i);
            while let Some(n) = s.node(c)? {
                self.set(c, n.text);
                c = c.parent();
            }
            self.add_message();
        }
        Ok(())
    }

    /// One `id+n|text` line per part, but none for tool calls.
    pub fn lines(&self) -> Vec<String> {
        self.view
            .iter()
            .filter(|&&c| self.text(c) != Some(""))
            .map(|&c| format!("{c}|{}", self.text(c).unwrap_or(PLACEHOLDER)))
            .collect()
    }

    pub fn render(&self) -> String {
        let mut s = String::from("<chat>\n");
        for l in self.lines() {
            s += &l;
            s.push('\n');
        }
        s + "</chat>"
    }
}
