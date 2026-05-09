## Release notes — baked providers refactor

### What changed

- `[[free_models]]` is removed. A config carrying that block fails to load with `UnknownKey { path: "free_models", ... }`. Transcribe each entry into `[[providers]]`.
- `[[providers]]` schema overhaul:
  - `vendor = "..."` → `subscription = "..."` (loader rejects the old key)
  - `cli = "..."` + `launch_name = "..."` → `launch = "<cli>/<launch_name>"` (single string, split on first `/`)
  - New optional key `quota_lookup_key = "..."` for entries that bill against a different quota row than their launch_name.
- `SubscriptionKind::Free` is removed. `SubscriptionKind::Direct` is added for untracked routes (own API key, self-hosted endpoint, unlimited contracts). `Direct` provider quota is `None` by default; set `quota_disabled = true` or `free = true` to force 100%.
- The baked table covers 30 hand-curated ipbr-listed models. Heuristic vendor inference (`infer_underlying_vendor_from_name`, `find_quota_by_heuristic`, the kimi-latest rewrite, etc.) is gone; an unbaked model with no user `[[providers]]` entry produces no candidate.
- The top model display shows `[deepseek] v4 flash`-style brackets — per-model curated brand (display_vendor) instead of subscription tag.

### Migration recipe

A legacy block:

```toml
[[free_models]]
mapped_into = "deepseek-v4-flash"
cli = "opencode"
model_name = "dsk-4-flash"
```

becomes:

```toml
[[providers]]
launch = "opencode/dsk-4-flash"
model = "deepseek-v4-flash"
subscription = "opencode-go"
free = true
```

For an own-API-key route via the claude CLI:

```toml
[[providers]]
launch = "claude/claude-opus-4-7"
model = "claude-opus-4-7"
subscription = "direct"
quota_disabled = true
```
