## Release notes — unified config file

### What changed

The per-subsystem `~/.codexize/ntfy.toml` and the scattered `const` knobs across
the codebase have been consolidated into a single **`~/.codexize/config.toml`**
(TOML 1.0, schema v1). Every operator-tunable knob — ntfy notification
parameters, ACP agent definitions, launch policy defaults, runner cadence,
directory paths, diagnostics, and memory budgets — now lives in one place and
has a baked default.

### No migration

The old `~/.codexize/ntfy.toml` is **not** automatically imported. If you were
using ntfy notifications you will need to mint a fresh topic:

```
codexize ntfy --reset
```

The loader treats a missing `~/.codexize/config.toml` as "use all defaults"
— nothing is auto-created on launch. To seed a fully-annotated file for hand
editing:

```
codexize config init
```

### Silent empty-topic default

The baked default for `ntfy.topic` is the **empty string**, which means
notifications are **quietly disabled** by default: no random topic is minted
on first launch, no ntfy traffic is produced, and the notification worker
starts but has nothing to deliver.

To arm notifications, either run `codexize ntfy --reset` (generates a fresh
topic and persists it) or set the topic via the CLI (`codexize config set
ntfy.topic <value>`) or the `:config` TUI panel inside a running session.

### Strict schema

Unknown keys and type mismatches are rejected with line/column error messages
— typos in `config.toml` refuse the binary to launch rather than silently
being ignored. The `meta.version` field gates future schema bumps.
