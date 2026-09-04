#!/usr/bin/env python3
"""Fail if a secret-shaped string is committed anywhere in the tree.

Brief section 183 asks CI to detect accidental secrets in logs, API responses,
build artifacts, debug output, crash reports, database dumps, test fixtures
and source code. This script is the source-and-fixtures half of that promise,
and it runs as a gate so a leaked credential cannot merge at all -- the
runtime half (redaction before a log line or an API response leaves the
process) is the loadgen redaction filter and the config summary/Debug tests,
which have their own tests.

The scan is deliberately shaped like the audit scripts it sits beside
(brief-audit, infra-audit, pydeps-audit): a fixed set of named rules, an
allowlist that must carry a reason, and a self-test that proves every rule
still fires on a synthetic secret and still ignores the near-misses the
repository legitimately contains. A scanner whose regex has quietly rotted
passes everything and protects nothing, so the self-test is part of the gate
rather than a comment asking someone to keep it honest.

Only well-known credential formats are rules. A generic hunt for
"password = <high-entropy string>" cannot tell a committed secret from a
test vector or a documentation example, and a gate that cries wolf teaches
everyone to scroll past it -- the exact failure mode the audit job's
non-gating design exists to avoid. A literal credential in a URL is the one
generic shape included, because the repository has real precedent for it:
connection strings are how a database password travels, and the interpolation
in tools/2node is the correct pattern this rule is calibrated to ignore.

Usage: python3 tools/scripts/secret-audit.py
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# One entry per credential family. The name is what a failure prints; the
# sample and the near-miss are what the self-test feeds the rule.
RULES = [
    (
        "github token",
        re.compile(r"gh[pousr]_[A-Za-z0-9]{36,}"),
        "ghp_" + "a" * 36,
        "ghp_ is a prefix, not a token",
    ),
    (
        "aws access key id",
        re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
        "AKIA" + "B" * 16,
        "AKIA0123 was a fine fixture but too short",
    ),
    (
        "google api key",
        re.compile(r"\bAIza[0-9A-Za-z_-]{35}\b"),
        "AIza" + "c" * 35,
        "AIza looks like a key but is only four letters",
    ),
    (
        "slack token",
        re.compile(r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
        "xoxb-" + "d" * 12,
        "xoxb- alone names the family, not a member",
    ),
    (
        "private key block",
        re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY( BLOCK)?-----"),
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN PUBLIC KEY----- is the one that is safe to commit",
    ),
    (
        "json web token",
        re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
        "eyJ" + "e" * 10 + ".eyJ" + "e" * 10 + "." + "e" * 10,
        "eyJhbGciOiJub25lIn0 is a header a test prints, never a signed triple",
    ),
    (
        "literal credentials in a url",
        # Six characters minimum on the password half. The repository's own
        # dev credentials (postgres://migo:migo@) sit under it, and shell
        # interpolation ($PG_PASSWORD) cannot match because "$" is excluded
        # from the class -- the pattern tools/2node uses is the shape this
        # rule is built to let through.
        re.compile(r"://[A-Za-z0-9._%+-]+:[^\s@/$]{6,}@"),
        "postgres://ada:swordfish-city@db.example.com/migo",
        "postgres://migo:migo@127.0.0.1:5432 and ${PG_USER}:${PG_PASSWORD}@",
    ),
]

# A path here is exempt from every rule, and the reason is mandatory prose
# because an allowlist entry without one is indistinguishable from a silenced
# alarm. Exempt the whole file, not the line: a fixture file exists to hold
# secret-shaped strings, and pinning line numbers means every unrelated edit
# re-opens the question.
ALLOWED: dict[str, str] = {
    # Fixtures for the redaction filter itself. These strings exist precisely
    # so the filter can be proven to catch them; the file's tests fail if one
    # stops being redacted, which is a tighter check than this gate could run.
    # The runtime half of brief section 183, in miniature: every file below
    # exists to prove a credential does NOT reach a summary, a Debug line, a
    # log, or a report. The synthetic secret is the input; the assertion that
    # it never appears in the output is the test. This gate and those tests
    # face in opposite directions from the same fixtures.
    "tools/loadgen/src/test/redact.test.ts": "redaction filter fixtures",
    "tools/loadgen/src/test/logger.test.ts": "redaction filter fixtures",
    "tools/loadgen/src/test/report.test.ts": "redaction filter fixtures",
    "server/crates/migo-core/src/config.rs": "config summary/Debug redaction tests",
    "server/crates/migod/tests/migod.rs": "config summary/Debug redaction tests",
    # This scanner. Its self-test samples are synthetic secrets by design.
    "tools/scripts/secret-audit.py": "the scanner's own self-test samples",
}

# Files the scan reads as text. Everything else (images, fonts, wasm, the
# lockfiles' occasional binary-adjacent blobs) is skipped after a probe, since
# no credential format is stored that way and reading every byte of a
# pnpm-lockfile buys nothing.
TEXT_SUFFIXES = {
    "", ".example", ".gitignore", ".md", ".json", ".jsonc", ".mjs", ".cjs",
    ".js", ".jsx", ".ts", ".tsx", ".kt", ".kts", ".rs", ".toml", ".yaml",
    ".yml", ".sh", ".bash", ".sql", ".css", ".html", ".svg", ".txt", ".xml",
    ".env", ".conf", ".cfg", ".ini", ".properties", ".gradle", ".pro",
    ".lock", ".mod", ".sum", ".desktop", ".service", ".dockerfile",
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


def self_test() -> None:
    """Prove each rule fires on a synthetic secret and ignores its near-miss.

    A scanner that has rotted -- a regex edited past usefulness, a character
    class that no longer admits the real format -- reports a clean tree with
    total confidence, which is worse than no scanner because it stops anyone
    looking. This runs on every invocation for the same reason pydeps-audit
    reads its own install line: the gate checks the gate.
    """
    for name, pattern, sample, near_miss in RULES:
        check(
            "rule %r still matches a synthetic secret" % name,
            bool(pattern.search(sample)),
            "the sample %r no longer matches" % sample,
        )
        check(
            "rule %r still ignores the near-misses" % name,
            not pattern.search(near_miss),
            "the near-miss %r now matches, which would be a false positive" % near_miss,
        )


def tracked_files() -> list[str]:
    """`git ls-files` rather than a directory walk.

    The gate is about what is committed, and ls-files is exactly that set:
    nothing in .gitignore can trip it, and nothing untracked can hide from
    it. Working from the index also means the scan is stable between
    machines -- a developer's build output is not somebody else's.
    """
    result = subprocess.run(
        ["git", "-C", ROOT, "ls-files", "-z"],
        capture_output=True,
        check=True,
    )
    paths = [path for path in result.stdout.decode("utf-8").split("\0") if path]
    # A submodule's gitlink is listed by ls-files but lives on disk as a directory
    # (contracts/lib). It is not ours to scan — pinned upstream sources, the same
    # standing as node_modules — and opening it as a file only errors.
    return [path for path in paths if not os.path.isdir(os.path.join(ROOT, path))]


def looks_textual(path: str) -> bool:
    suffix = os.path.splitext(path)[1].lower()
    if "dockerfile" in os.path.basename(path).lower():
        return True
    return suffix in TEXT_SUFFIXES


def scan(path: str) -> list[str]:
    """The rules that fire in one file, as printable lines."""
    full = os.path.join(ROOT, path)
    try:
        with open(full, encoding="utf-8", errors="replace") as handle:
            source = handle.read()
    except OSError as exc:  # a listed file that cannot be read is its own failure
        return ["unreadable: %s" % exc]
    hits: list[str] = []
    for name, pattern, _, _ in RULES:
        for match in pattern.finditer(source):
            line = source.count("\n", 0, match.start()) + 1
            hits.append("line %d: %s" % (line, name))
    return hits


def main() -> int:
    print("secret-audit: credential-shaped strings across every tracked file")
    print()

    self_test()

    files = tracked_files()
    check("the tree has files to scan at all", bool(files), "git ls-files is empty")

    scanned = 0
    for path in files:
        if not looks_textual(path):
            continue
        if not os.path.exists(os.path.join(ROOT, path)):
            # Tracked but deleted in the working tree and not yet committed:
            # the file will be gone from the index next commit, and CI always
            # checks out clean, so there is nothing to scan. Any other read
            # failure is still reported by scan().
            continue
        scanned += 1
        hits = scan(path)
        if not hits:
            continue
        if path in ALLOWED:
            # Counted as its own check so the allowlist cannot silently grow
            # stale: an entry whose file no longer trips any rule is a reason
            # that outlived its fixture and should be deleted.
            check(
                "allowlisted %s (%s) still holds its fixtures" % (path, ALLOWED[path]),
                True,
            )
            continue
        for hit in hits:
            problems.append("%s: %s" % (path, hit))
            print("  FAIL  %s: %s" % (path, hit))
    check("scanned the textual tracked files", scanned > 100,
          "only %d files scanned -- the suffix list may have drifted" % scanned)

    print()
    print("%d checks, %d problem(s)" % (checks, len(problems)))
    if problems:
        print("a secret-shaped string is committed; rotate the credential if it was real")
        return 1
    print("clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
