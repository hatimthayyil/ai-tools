# Rust retry crates

For: bounding the compactor's retries (`ai memory nap` retried a failing node every 10 s forever). Need: exponential backoff with jitter and a cap on attempts, usable today as a plain iterator of delays (the compactor has threads and its own `waits` queue, no Tokio), with async retry for Tokio if the compactor moves there.

Checked 2026-10-08 (crates.io API, GitHub, RustSec advisory-db).

| crate | latest | last release | downloads (90 d) | status |
|---|---|---|---|---|
| backon | 1.6.0 | 2025-10 | 33.3 M | active (Xuanwo; used by Apache OpenDAL), repo pushed 2026-06 |
| backoff | 0.4.0 | 2021-12 | 16.0 M | unmaintained: RUSTSEC-2025-0012, which recommends backon |
| retry | 2.2.0 | 2026-01 | 4.6 M | maintained, small; sync only |
| tokio-retry | 0.3.2 | 2026-06 | 9.6 M | revived (djc); Tokio only |
| tokio-retry2 | 0.9.1 | 2026-01 | 6.1 M | fork of tokio-retry; Tokio only |

- backon: `ExponentialBuilder` (min/max delay, factor, `with_max_times`, `with_total_delay`, `with_jitter`) builds an `ExponentialBackoff` that is an `Iterator<Item = Duration>`; `None` = give up. Also `Retryable` (async, Tokio/gloo/embassy/futures-timer sleepers by feature) and `BlockingRetryable` (sync). `default-features = false, features = ["std"]` pulls only `fastrand` (already in the tree). Jitter adds 0-100% of the current delay.
- backoff: iterator-like `Backoff::next_backoff`, async via `future` feature. Unmaintained; flagged by `cargo audit`.
- retry: delay iterators (`Exponential`, `Fixed`, `jitter` map) and a blocking `retry` fn. No async: a move to Tokio means a second crate.
- tokio-retry / tokio-retry2: strategies are iterators too, but the crate depends on Tokio; using only its iterators today adds Tokio for nothing.

Choice: backon. Maintained, the most used, recommended by the advisory that retires `backoff`; its backoff is a plain iterator that plugs into `waits` unchanged; and its async `Retryable` with the `tokio-sleep` feature covers a later move to Tokio without changing crates.

Use in `ai memory`: one `ExponentialBackoff` per failing node, 10 s doubling to 5 min, jitter, 8 retries (20-40 min in all), then the nap gives up (ERROR `gave up` in nap.log, exit 1); the next note's nap retries. Details in `ai/comments/ai/src/rs/memory/src/compact.rs.md`.
