use crate::memory::tree::Coord;

/// Tiles `[0, t)` with aligned blocks, keeping a block whole iff it fits in the log and its
/// size is at most `alpha` times its age: bigger alpha, coarser tiling.
fn tile(t: u64, alpha: f64) -> Vec<Coord> {
    let mut l = 0;
    while (1u64 << l) < t {
        l += 1;
    }
    let (mut out, mut stack) = (Vec::new(), vec![Coord::new(l, 0)]);
    while let Some(c) = stack.pop() {
        if c.id() >= t {
            continue;
        }
        if c.l > 0 && (c.end() > t || c.n() as f64 > alpha * (t - c.id()) as f64) {
            let [a, b] = c.children();
            stack.push(b);
            stack.push(a);
        } else {
            out.push(c);
        }
    }
    out
}

/// What `view` prints (OptMem's cover): at most `budget` blocks tiling `[0, t)`, oldest
/// first, detail fading with age. Lines the budget leaves over go to the newest blocks.
pub fn cover(t: u64, budget: u64) -> Vec<Coord> {
    if t <= budget {
        return (0..t).map(Coord::leaf).collect();
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if tile(t, mid).len() as u64 > budget {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let mut out = tile(t, hi);
    while (out.len() as u64) < budget {
        let Some(k) = out.iter().rposition(|c| c.l > 0) else {
            break;
        };
        out.splice(k..=k, out[k].children());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiles(cs: &[Coord], t: u64) -> bool {
        cs.first().is_none_or(|c| c.id() == 0)
            && cs.windows(2).all(|w| w[0].end() == w[1].id())
            && cs.last().is_none_or(|c| c.end() == t)
    }

    #[test]
    fn small_logs_are_whole() {
        assert_eq!(
            cover(3, 96),
            [Coord::leaf(0), Coord::leaf(1), Coord::leaf(2)]
        );
        assert!(cover(0, 96).is_empty());
    }

    #[test]
    fn covers_fit_the_budget_and_fade_with_age() {
        for t in [97, 100, 255, 1000, 4097, 1_000_000] {
            for budget in [8, 96, 300] {
                let c = cover(t, budget);
                assert!(tiles(&c, t), "t={t} budget={budget}");
                assert_eq!(c.len() as u64, budget.min(t), "t={t} budget={budget}");
                assert!(c[0].l >= c[c.len() - 1].l, "older lines cover more");
            }
        }
    }
}
