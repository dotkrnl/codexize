//! Annotated full-defaults dump used by `codexize config defaults` and
//! `codexize config init`. The on-disk file is otherwise sparse; this is
//! the one place the operator sees every key inline with its baked value.
//!
//! Output is deliberately hand-rolled (instead of round-tripped through
//! `toml_edit`) so the section header comments and inline notes stay
//! exactly where the spec lays them out.

use std::fmt::Write as _;

use super::fmt::{format_inline_env, format_string_array, toml_quote as quote};
use super::schema::{AcpAgentSection, Config};

/// Render the canonical fully-populated annotated TOML for `config`.
/// Parsing this output back through [`super::loader::load_str`] must
/// produce a `Config` semantically equal to the input (round-trip
/// invariant — see loader tests).
pub fn emit_annotated(config: &Config) -> String {
    // `writeln!` against `String` is infallible (the `Write` impl never
    // returns an error), so every call below uses `.ok()` to discard the
    // `Result` without tripping `unused_must_use` or the boundary lint
    // that bans bare unwrap in production source.
    let mut out = String::new();
    out.push_str("# codexize unified config (schema v1).\n");
    out.push_str("# Sparse on disk: `set`/`unset` and TUI save drop keys equal to defaults.\n");
    out.push_str("# `codexize config defaults` always prints this fully-populated form.\n");
    out.push('\n');

    out.push_str("[meta]\n");
    writeln!(out, "version = {}", config.meta.version).ok();
    out.push('\n');

    let n = &config.ntfy;
    out.push_str("[ntfy]\n");
    writeln!(out, "enabled = {}", n.enabled.value()).ok();
    writeln!(out, "server = {}", quote(n.server.value())).ok();
    out.push_str("# topic empty disables notifications; mint via `codexize ntfy --reset`.\n");
    writeln!(out, "topic = {}", quote(n.topic.value())).ok();
    writeln!(
        out,
        "detail_mode = {}",
        quote(n.detail_mode.value().as_str())
    )
    .ok();
    writeln!(out, "retry_attempts = {}", n.retry_attempts.value()).ok();
    writeln!(out, "retry_delay_ms = {}", n.retry_delay_ms.value()).ok();
    writeln!(out, "http_timeout_secs = {}", n.http_timeout_secs.value()).ok();
    writeln!(out, "body_max_bytes = {}", n.body_max_bytes.value()).ok();
    writeln!(out, "excerpt_max_chars = {}", n.excerpt_max_chars.value()).ok();
    if let Some(ts) = n.created_at.value() {
        writeln!(out, "created_at = {}", quote(&ts.to_rfc3339())).ok();
    }
    if let Some(ts) = n.updated_at.value() {
        writeln!(out, "updated_at = {}", quote(&ts.to_rfc3339())).ok();
    }
    out.push('\n');

    out.push_str("[ntfy.events]\n");
    writeln!(out, "stage_wait = {}", n.events.stage_wait.value()).ok();
    writeln!(
        out,
        "interactive_wait = {}",
        n.events.interactive_wait.value()
    )
    .ok();
    writeln!(out, "pipeline_done = {}", n.events.pipeline_done.value()).ok();
    out.push('\n');

    let p = &config.acp.policy;
    out.push_str("[acp.policy]\n");
    writeln!(
        out,
        "shell_policy = {}",
        quote(p.shell_policy.value().as_str())
    )
    .ok();
    writeln!(
        out,
        "shell_allowlist = {}",
        format_string_array(p.shell_allowlist.value())
    )
    .ok();
    writeln!(
        out,
        "enforce_readonly_workspace = {}",
        p.enforce_readonly_workspace.value()
    )
    .ok();
    writeln!(
        out,
        "allowed_write_paths = {}",
        format_string_array(p.allowed_write_paths.value())
    )
    .ok();
    out.push('\n');

    let i = &config.acp.install;
    out.push_str("[acp.install]\n");
    writeln!(
        out,
        "claude_acp_root = {}",
        quote(i.claude_acp_root.value())
    )
    .ok();
    writeln!(
        out,
        "prefer_local_claude_acp = {}",
        i.prefer_local_claude_acp.value()
    )
    .ok();
    out.push('\n');

    out.push_str("# Per-vendor ACP launch knobs. `program` is the executable used\n");
    out.push_str("# when no local install is detected. `enabled = false` removes the vendor\n");
    out.push_str("# from `available_clis()`. Keys under `env` populate the spawn\n");
    out.push_str("# environment as a base; system `CODEXIZE_ACP_*` keys overwrite collisions.\n");
    for (vendor, agent) in [
        ("claude", &config.acp.agents.claude),
        ("codex", &config.acp.agents.codex),
        ("gemini", &config.acp.agents.gemini),
        ("kimi", &config.acp.agents.kimi),
        ("opencode", &config.acp.agents.opencode),
    ] {
        emit_agent(&mut out, vendor, agent);
    }

    let r = &config.runner;
    out.push_str("[runner]\n");
    writeln!(
        out,
        "full_review_interval = {}",
        r.full_review_interval.value()
    )
    .ok();
    out.push('\n');

    let pths = &config.paths;
    out.push_str("[paths]\n");
    writeln!(out, "cache_root = {}", quote(pths.cache_root.value())).ok();
    writeln!(out, "sessions_root = {}", quote(pths.sessions_root.value())).ok();
    writeln!(out, "runs_root = {}", quote(pths.runs_root.value())).ok();
    writeln!(out, "memory_root = {}", quote(pths.memory_root.value())).ok();
    out.push('\n');

    let u = &config.ui;
    out.push_str("[ui]\n");
    writeln!(
        out,
        "prefer_split_on_open = {}",
        u.prefer_split_on_open.value()
    )
    .ok();
    out.push('\n');
    out.push_str("[ui.colon_palette]\n");
    writeln!(out, "show_help = {}", u.colon_palette.show_help.value()).ok();
    out.push('\n');
    out.push_str("[ui.footer]\n");
    writeln!(out, "show_keys = {}", u.footer.show_keys.value()).ok();
    out.push('\n');

    let d = &config.diagnostics;
    out.push_str("[diagnostics]\n");
    writeln!(out, "log_level = {}", quote(d.log_level.value().as_str())).ok();
    writeln!(out, "json_logs = {}", d.json_logs.value()).ok();
    out.push('\n');

    let m = &config.memory;
    out.push_str("[memory]\n");
    writeln!(out, "enabled = {}", m.enabled.value()).ok();
    writeln!(
        out,
        "max_topics_per_read = {}",
        m.max_topics_per_read.value()
    )
    .ok();
    writeln!(
        out,
        "journal_retention_months = {}",
        m.journal_retention_months.value()
    )
    .ok();

    out
}

fn emit_agent(out: &mut String, vendor: &str, agent: &AcpAgentSection) {
    out.push('\n');
    writeln!(out, "[acp.agents.{vendor}]").ok();
    writeln!(out, "enabled = {}", agent.enabled.value()).ok();
    writeln!(out, "program = {}", quote(agent.program.value())).ok();
    writeln!(out, "args = {}", format_string_array(agent.args.value())).ok();
    writeln!(out, "env = {}", format_inline_env(agent.env.value())).ok();
}
