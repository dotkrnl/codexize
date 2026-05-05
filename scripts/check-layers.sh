#!/usr/bin/env bash
set -euo pipefail

root="${1:-.}"

python3 - "$root" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(sys.argv[1])
SRC = ROOT / "src"

LAYER_RULES = {
    "logic": [
        ("terminal UI", re.compile(r"\b(?:ratatui|crossterm)\b")),
        (
            "backend IO/process",
            re.compile(r"\b(?:std::fs|std::process|tokio::fs|notify|portable_pty|reqwest)\b"),
        ),
        (
            "backend cache/dashboard/quota IO",
            # Forbids the IO entrypoints regardless of which alias the call
            # site uses (`crate::cache::load`, `cache::save_dashboard`,
            # `dashboard::load_models`, `quota::load_quota_maps_for`, …).
            # Type imports such as `cache::DashboardEntry` or
            # `cache::LoadedSection` are intentionally NOT matched —
            # only the IO-performing functions are.
            re.compile(
                r"\b(?:cache::(?:load\b|save_(?:dashboard|quotas|quota_resets))"
                r"|dashboard(?:_io)?::load_models"
                r"|load_quota_maps(?:_for)?)\b"
            ),
        ),
    ],
    "data": [
        ("terminal UI", re.compile(r"\b(?:ratatui|crossterm)\b")),
    ],
    "ui": [
        (
            "backend IO/process",
            re.compile(r"\b(?:std::fs|std::process|tokio::fs|notify|portable_pty|reqwest)\b"),
        ),
    ],
}


def strip_cfg_test_modules(text: str) -> str:
    lines = text.splitlines(keepends=True)
    out: list[str] = []
    pending_cfg_test = False
    skip_depth: int | None = None

    for line in lines:
        stripped = line.strip()
        if skip_depth is not None:
            skip_depth += line.count("{") - line.count("}")
            if skip_depth <= 0:
                skip_depth = None
            out.append("\n")
            continue

        if stripped.startswith("#[cfg(test)]"):
            pending_cfg_test = True
            out.append("\n")
            continue

        if pending_cfg_test and re.match(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{", stripped):
            skip_depth = line.count("{") - line.count("}")
            pending_cfg_test = False
            out.append("\n")
            if skip_depth <= 0:
                skip_depth = None
            continue

        pending_cfg_test = False
        out.append(line)

    return "".join(out)


def iter_rust_files(layer: str):
    layer_dir = SRC / layer
    if not layer_dir.exists():
        return
    for path in sorted(layer_dir.rglob("*.rs")):
        # Co-located test files declared as `#[cfg(test)] mod tests_*;` are
        # always test-only by convention. Spec allows fixture/tempdir IO in
        # cfg(test) modules; treat the file equivalents the same way the
        # inline `#[cfg(test)] mod foo {}` stripper does.
        if path.name.startswith("tests_") or path.name == "tests.rs":
            continue
        yield path


violations: list[str] = []
for layer, rules in LAYER_RULES.items():
    for path in iter_rust_files(layer) or []:
        text = strip_cfg_test_modules(path.read_text(encoding="utf-8"))
        for lineno, line in enumerate(text.splitlines(), start=1):
            if not line.strip() or line.lstrip().startswith("//"):
                continue
            for label, pattern in rules:
                match = pattern.search(line)
                if match:
                    rel = path.relative_to(ROOT)
                    violations.append(
                        f"{rel}:{lineno}: {layer} layer must not reference {label}: {match.group(0)}"
                    )

if violations:
    print("Layer-boundary violations:", file=sys.stderr)
    for violation in violations:
        print(f"  {violation}", file=sys.stderr)
    sys.exit(1)
PY
