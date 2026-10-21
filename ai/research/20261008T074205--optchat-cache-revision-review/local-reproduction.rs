// Author: @codex (GPT-6). Date: 2026-10-08.
// Changelog: 2026-10-08: deterministic production-source fold/restart reproduction.
// Deterministic cache-shape audit using the unchanged production Mem implementation.
// No database, network, models, or API calls. Small budgets expose folding behaviour.
extern crate self as anyhow;
pub type Result<T> = std::result::Result<T, std::io::Error>;
mod tree { include!("/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/tree.rs"); }
mod store {
    use crate::tree::Coord;
    pub struct Snapshot;
    pub struct Node { pub text: String }
    impl Snapshot {
        pub fn log_len(&self) -> anyhow::Result<u64> { unreachable!() }
        pub fn level(&self, _: u32) -> anyhow::Result<Vec<Option<Node>>> { unreachable!() }
        pub fn node(&self, _: Coord) -> anyhow::Result<Option<Node>> { unreachable!() }
    }
}
mod view { include!("/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs"); }
use tree::Coord;
use view::Mem;
fn main() {
    let pairs = [(0u64,4u64), (8,1)];
    for (id,n) in pairs {
        let old = (10-id) as f64 / (4*n) as f64;
        let new = (10-(id+2*n-1)) as f64 / n as f64;
        println!("T=10 pair {id}+{n} and {}+{n}: old={old:.3} new={new:.3}", id+n);
    }
    let mut m = Mem::new(30);
    for i in 0..20 {
        let mut c = Coord::leaf(i);
        loop {
            m.set(c, "abcdefghij".into());
            if c.l == 0 && c.i % 2 == 0 { break; }
            c = c.parent();
            if c.end() > i+1 { break; }
        }
        let before = m.view().to_vec();
        m.add_message();
        let mut copy = m.clone(); copy.refold();
        println!("t={} size={} view={:?} changed_old_lines={} refold_equal={}",i+1,m.size(),m.view(),before.iter().any(|c| !m.view().contains(c)),copy.view()==m.view());
    }
    // A live run waits for parents; restart sees all parents already available.
    let mut delayed = Mem::new(30);
    for i in 0..20 { delayed.set(Coord::leaf(i), "abcdefghij".into()); delayed.add_message(); }
    for l in 1..=4 { for i in 0..20/(1<<l) { delayed.add_node(Coord::new(l,i),"abcdefghij".into()); } }
    let live = delayed.view().to_vec();
    delayed.refold();
    println!("late parents: live={live:?}; refold={:?}; equal={}",delayed.view(),live==delayed.view());
}
