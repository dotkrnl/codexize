#!/usr/bin/env bash
set -euo pipefail

python3 - "$@" <<'PY'
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("root", nargs="?", default=".")
parser.add_argument("--warn", action="store_true")
parser.add_argument("--strict", action="store_true")
args = parser.parse_args()

ROOT = Path(args.root)
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

def get_rules_for_path(path: Path) -> list[tuple[str, re.Pattern]]:
    try:
        rel_path = path.relative_to(SRC)
    except ValueError:
        return []
        
    parts = rel_path.parts
    if not parts:
        return []
        
    rules = []
    
    # 1. Existing layer-based rules
    if parts[0] == "logic":
        rules.extend(LAYER_RULES["logic"])
    elif parts[0] == "data":
        rules.extend(LAYER_RULES["data"])
    elif parts[0].startswith("ui"):
        # src/ui_headless is exempt from TUI-specific terminal-library rules
        # (it is not the TUI) but still forbidden from backend IO.
        rules.extend(LAYER_RULES["ui"])
        
    # 2. New boundary rules for ui* (src/ui, src/ui_headless, src/ui_*)
    if parts[0].startswith("ui"):
         # Forbid app/app_shell imports
         rules.append(("app import", re.compile(r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?use\s+crate::(?:app|app_shell)(?:::|\s*;)")))
         
    # 3. New boundary rules for app* (src/app, src/app_shell.rs, src/app_runtime)
    is_app_layer = parts[0] in ("app", "app_runtime") or (len(parts) == 1 and parts[0] == "app_shell.rs")
    if is_app_layer:
        # Forbid ui imports
        rules.append(("ui import", re.compile(r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?use\s+crate::ui(?:::|\s*;)")))
        # AC-2: Terminal types crossing into app*
        rules.append(("terminal UI types", re.compile(r"\b(?:ratatui|crossterm|KeyEvent)\b")))
    
    # 4. AC-2: raw key types in app_runtime
    if parts[0] == "app_runtime":
        rules.append(("raw key types", re.compile(r"\b(?:KeyPress|UiKey|UiKeyCode)\b")))

    # 5. Global rule: pub use crate::ui:: outside ui*
    if not parts[0].startswith("ui"):
        rules.append(("ui re-export", re.compile(r"^\s*pub\s*(?:\([^)]*\))?\s*use\s+crate::ui(?:::|\s*;)")))

    return rules

def iter_rust_files():
    for path in sorted(SRC.rglob("*.rs")):
        if path.name.startswith("tests_") or path.name.endswith("_tests.rs") or path.name == "tests.rs":
            continue
        yield path

violations: list[str] = []
for path in iter_rust_files():
    rules = get_rules_for_path(path)
    if not rules:
        continue
        
    text = strip_cfg_test_modules(path.read_text(encoding="utf-8"))
    for lineno, line in enumerate(text.splitlines(), start=1):
        if not line.strip() or line.lstrip().startswith("//"):
            continue
        for label, pattern in rules:
            match = pattern.search(line)
            if match:
                rel = path.relative_to(ROOT)
                violations.append(
                    f"{rel}:{lineno}: {label} violation: {match.group(0)}"
                )

if violations:
    print("Layer-boundary violations:", file=sys.stderr)
    for violation in violations:
        print(f"  {violation}", file=sys.stderr)
    
    if args.strict:
        sys.exit(1)
    elif args.warn:
        sys.exit(0)
    else:
        # Default behavior: if neither --warn nor --strict is passed, 
        # exit non-zero on violations (as before).
        sys.exit(1)
PY
