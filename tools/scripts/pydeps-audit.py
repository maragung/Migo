#!/usr/bin/env python3
"""Check that the CI gate installs exactly the third-party Python modules it imports.

This gate exists because of a specific failure, and the failure is worth writing down.
The gate job pins an interpreter with actions/setup-python so that `pip install` is
allowed at all -- the distribution Python on the runner image is an externally-managed
environment and refuses it. Pinning an interpreter also swaps out whatever the image
happened to pre-install. `cryptography` was one of those: the crypto vector generator
imported it, the image shipped it, nobody declared it, and the gate went red the moment
an unrelated step pinned a different Python.

So the pip list in the workflow is a declaration, and a declaration that nothing checks
drifts. This script reads both sides -- the imports under tools/ and the install line in
the workflow -- and refuses a mismatch in either direction. A module imported but not
installed is the failure above. A module installed but not imported is the reverse: an
install line that outlived its reason, which is how the next reader learns to distrust
the list. Both are reported.

It deliberately needs nothing outside the standard library, so it can be the one gate
that still runs when the environment it is checking is broken.

Usage: python3 tools/scripts/pydeps-audit.py
"""

from __future__ import annotations

import ast
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TOOLS = os.path.join(ROOT, "tools")
WORKFLOW = os.path.join(ROOT, ".github", "workflows", "ci.yml")

SKIP_DIRS = {"node_modules", "dist", "build", "target", "__pycache__", ".venv", "venv"}

# Import name on the left, the name pip knows it by on the right. There is no programmatic
# way to derive one from the other, so an unknown import is a hard failure rather than a
# guess: adding a dependency should be a deliberate two-line edit, not a silent inference.
DISTRIBUTION = {
    "cryptography": "cryptography",
    "yaml": "pyyaml",
}

problems: list[str] = []
checks = 0


def check(label: str, ok: bool, detail: str = "") -> None:
    global checks
    checks += 1
    if ok:
        print("  ok    " + label)
    else:
        print("  FAIL  " + label + ((": " + detail) if detail else ""))
        problems.append(label + ((": " + detail) if detail else ""))


def python_files() -> list[str]:
    out = []
    for base, dirs, files in os.walk(TOOLS):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS and not d.startswith("."))
        for name in sorted(files):
            if name.endswith(".py"):
                out.append(os.path.join(base, name))
    return out


def local_module_names() -> set[str]:
    """Every module name that resolves inside the repo, so a sibling import is not
    mistaken for a package that has to come from an index."""
    names = set()
    for base, dirs, files in os.walk(ROOT):
        dirs[:] = [d for d in dirs if d not in SKIP_DIRS and not d.startswith(".")]
        for name in files:
            if name.endswith(".py"):
                names.add(name[:-3])
        for d in dirs:
            if os.path.exists(os.path.join(base, d, "__init__.py")):
                names.add(d)
    return names


def imported_modules(paths: list[str], local: set[str]) -> dict[str, list[str]]:
    """Top-level import names that are neither standard library nor local, mapped to the
    files that import them."""
    stdlib = set(sys.stdlib_module_names)
    found: dict[str, list[str]] = {}
    for path in paths:
        with open(path, encoding="utf-8") as handle:
            source = handle.read()
        try:
            tree = ast.parse(source, path)
        except SyntaxError as exc:  # a gate script that will not parse is its own failure
            problems.append("%s does not parse: %s" % (os.path.relpath(path, ROOT), exc))
            continue
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                candidates = [alias.name for alias in node.names]
            elif isinstance(node, ast.ImportFrom):
                # A relative import names nothing that pip could install.
                candidates = [] if node.level else [node.module or ""]
            else:
                continue
            for candidate in candidates:
                top = candidate.split(".")[0]
                if not top or top in stdlib or top in local:
                    continue
                found.setdefault(top, []).append(os.path.relpath(path, ROOT))
    return found


def installed_packages() -> tuple[set[str], str]:
    """The distributions the gate job installs, read out of the workflow itself."""
    with open(WORKFLOW, encoding="utf-8") as handle:
        workflow = handle.read()
    installs = re.findall(r"(?m)^\s*(?:run:\s*)?python3 -m pip install ([^\n]+)$", workflow)
    packages: set[str] = set()
    for line in installs:
        for word in line.split():
            if word.startswith("-"):
                continue
            packages.add(re.split(r"[=<>!\[]", word)[0].lower())
    return packages, "\n".join(installs)


def main() -> int:
    print("pydeps-audit: tools/ imports against the CI gate install line")
    print()

    files = python_files()
    check("tools/ has Python to check at all", bool(files), "no .py files found")

    imports = imported_modules(files, local_module_names())
    packages, raw = installed_packages()
    check("the gate job installs Python packages somewhere", bool(packages), "no pip install line in " + os.path.relpath(WORKFLOW, ROOT))

    unknown = sorted(name for name in imports if name not in DISTRIBUTION)
    check(
        "every third-party import has a known distribution name",
        not unknown,
        "add %s to DISTRIBUTION in this script, imported by %s"
        % (", ".join(unknown), ", ".join(sorted({f for name in unknown for f in imports[name]}))),
    )

    for name in sorted(imports):
        dist = DISTRIBUTION.get(name)
        if dist is None:
            continue
        check(
            "%s (imported by %s) is installed as %s" % (name, ", ".join(sorted(set(imports[name]))), dist),
            dist.lower() in packages,
            "the gate installs [%s] but not %s" % (", ".join(sorted(packages)), dist),
        )

    wanted = {DISTRIBUTION[name].lower() for name in imports if name in DISTRIBUTION}
    stale = sorted(packages - wanted)
    check(
        "the gate installs nothing it no longer imports",
        not stale,
        "%s is installed but nothing under tools/ imports it" % ", ".join(stale),
    )

    print()
    print("%d checks, %d problem(s)" % (checks, len(problems)))
    if problems:
        print("the install line in .github/workflows/ci.yml and the imports under tools/ disagree")
        return 1
    print("clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
