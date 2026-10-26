"""DeepSeek list prices, from api-docs.deepseek.com/quick_start/pricing (fetched 2026-10-26)."""

from datetime import datetime, timezone

# Per 1M tokens at peak: cache hit, cache miss, output. Off-peak is half.
PEAK = {"deepseek-flash": (0.006, 0.30, 1.20), "deepseek-v4-pro": (0.044, 1.32, 3.96)}


def peak(created):
    """Peak is 01:00-04:00 and 06:00-10:00 UTC, Monday to Friday (Chinese holidays aside)."""
    t = datetime.fromtimestamp(created, timezone.utc)
    return t.weekday() < 5 and (1 <= t.hour < 4 or 6 <= t.hour < 10)


def cost(model, created, usage):
    """The USD a reply cost, from its `usage` (reasoning tokens count as output)."""
    hit, miss, out = (p if peak(created) else p / 2 for p in PEAK[model])
    h = usage.get("prompt_cache_hit_tokens", 0)
    m = usage.get("prompt_cache_miss_tokens", usage["prompt_tokens"] - h)
    return (h * hit + m * miss + usage["completion_tokens"] * out) / 1e6
