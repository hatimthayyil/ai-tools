use ai_tools::memory::{Coord,cover};
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

fn main(){for n in [1_000,10_000,20_000]{let t=std::time::Instant::now();let v=fold(n,64,|_|1);println!("fold({n}): {:?}, {} lines",t.elapsed(),v.len());let t=std::time::Instant::now();let v=cover(n,64);println!("cover({n}): {:?}, {} lines",t.elapsed(),v.len());}}
