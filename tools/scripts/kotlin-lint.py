#!/usr/bin/env python3
"""Static checks on the Android client's Kotlin, for a sandbox with no JVM.

Nothing in this repository can compile Kotlin locally: the module needs a JDK, the
Android SDK and Gradle, and .github/workflows/android.yml is the only place all three
exist. Every mistake in this tree therefore costs a push and a runner, and a mistake
that hides the rest of the file costs several of them in a row.

This is not a type checker and cannot be one without the classpath. It checks the small
set of properties that are cheap to verify from the text and expensive to discover from
a runner:

1. Block comments must balance. Kotlin block comments NEST, unlike Java's and C's, so
   `/* /* */` needs two closers. A two-character `/*` in KDoc prose -- a shell glob
   written as a path with a star, a package written with a star -- silently opens a
   second comment and swallows the rest of the file. The compiler reports it at EOF as a
   bare "Unclosed comment", thousands of unresolved references away from the prose that
   caused it. This exact mistake in two files produced 507 errors in one CI run and hid
   every other problem in the module behind them.

2. Cyrillic in source. U+0400-U+04FF contains a/e/o/c/p/x glyphs identical to the Latin
   ones, so a copy-paste can silently produce a different identifier that reads right
   and resolves to nothing. Nothing in review catches it.

3. Imports: unused, duplicated, or out of order. The compiler warns about unused imports
   but android.yml does not fail on warnings (deliberately -- see its header), so they
   accumulate unnoticed, and there is no ktlint or detekt to sort them.

4. Trailing whitespace, because it is free to check.

Line length is NOT checked: protocol/Generated.kt is generator output and its lines are
long by construction, and a gate that demands edits to generated files is a gate someone
will delete.

Exit status is 1 if anything is reported, so this can be a Makefile gate.
"""

import os
import re
import sys

# Kotlin resolves these through the `by` operator, so the imported name never appears
# again in the file and no textual scan can see the use. Removing them breaks the build;
# reporting them trains people to ignore this script. See :app's Compose screens.
DELEGATE_IMPORTS = frozenset(
    {
        "androidx.compose.runtime.getValue",
        "androidx.compose.runtime.setValue",
    }
)

CYRILLIC = re.compile("[Ѐ-ӿ]")

IMPORT = re.compile(r"^import\s+([\w.]+(?:\.\*)?)\s*(?:as\s+(\w+))?\s*$")

SKIP_DIRS = frozenset({"build", ".gradle", ".kotlin", ".idea"})


def kotlin_files(roots):
    for root in roots:
        if os.path.isfile(root):
            yield root
            continue
        for dirpath, dirs, files in os.walk(root):
            dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
            for name in sorted(files):
                if name.endswith(".kt") or name.endswith(".kts"):
                    yield os.path.join(dirpath, name)


def check_comments(path, src, report):
    """Reports a block comment that never closes, naming the line that opened it.

    Line comments and string literals are skipped: a `/*` inside either is inert, and
    reporting one would bury the finding this check exists for.
    """
    i = 0
    line = 1
    opens = []
    n = len(src)
    while i < n:
        char = src[i]
        if char == "\n":
            line += 1
            i += 1
        elif opens:
            if src.startswith("/*", i):
                opens.append(line)
                i += 2
            elif src.startswith("*/", i):
                opens.pop()
                i += 2
            else:
                i += 1
        elif src.startswith("//", i):
            newline = src.find("\n", i)
            i = n if newline < 0 else newline
        elif src.startswith("/*", i):
            opens.append(line)
            i += 2
        elif src.startswith('"""', i):
            end = src.find('"""', i + 3)
            if end < 0:
                break
            line += src.count("\n", i, end + 3)
            i = end + 3
        elif char in "\"'":
            i += 1
            while i < n and src[i] != char:
                if src[i] == "\\":
                    i += 1
                elif src[i] == "\n":
                    line += 1
                i += 1
            i += 1
        else:
            i += 1
    for opened in opens:
        report(
            path,
            opened,
            "block comment is never closed. Kotlin block comments nest, so a slash-star "
            "sequence in prose (a glob, a package name) opens one",
        )


def check_text(path, lines, report):
    for number, text in enumerate(lines, start=1):
        stripped = text.rstrip("\n")
        if stripped != stripped.rstrip():
            report(path, number, "trailing whitespace")
        found = CYRILLIC.search(stripped)
        if found:
            report(
                path,
                number,
                "Cyrillic U+%04X looks like a Latin letter but is not" % ord(found.group()),
            )


def check_imports(path, lines, report):
    imports = []
    for number, text in enumerate(lines, start=1):
        match = IMPORT.match(text.strip())
        if match:
            imports.append((number, match.group(1), match.group(2)))
    if not imports:
        return

    seen = {}
    for number, name, _alias in imports:
        if name in seen:
            report(path, number, "duplicate import of %s (first at line %d)" % (name, seen[name]))
        else:
            seen[name] = number

    names = [name for _n, name, _a in imports]
    for index in range(1, len(names)):
        if names[index] < names[index - 1]:
            report(
                path,
                imports[index][0],
                "imports are not sorted: %s follows %s" % (names[index], names[index - 1]),
            )
            break

    # Strip the import lines before looking for uses, so an import cannot vouch for
    # itself, and strip comments so a passing mention in prose does not either -- except
    # that a KDoc `[Symbol]` reference IS a use, so collect those first.
    numbers = {number for number, _n, _a in imports}
    body = "".join(t for n, t in enumerate(lines, start=1) if n not in numbers)
    kdoc = {ref.split(".")[0] for ref in re.findall(r"\[([\w.]+)", body)}
    code = re.sub(r"/\*.*?\*/", " ", body, flags=re.S)
    code = re.sub(r"//[^\n]*", " ", code)

    for number, name, alias in imports:
        if name in DELEGATE_IMPORTS or name.endswith(".*"):
            continue
        symbol = alias or name.rsplit(".", 1)[-1]
        if re.search(r"\b%s\b" % re.escape(symbol), code) or symbol in kdoc:
            continue
        report(path, number, "unused import %s" % name)


def main(argv):
    roots = argv[1:] or ["clients/android"]
    problems = []

    def report(path, line, message):
        problems.append("%s:%d %s" % (path, line, message))

    checked = 0
    for path in kotlin_files(roots):
        checked += 1
        with open(path, encoding="utf-8") as handle:
            src = handle.read()
        lines = src.splitlines(keepends=True)
        check_comments(path, src, report)
        check_text(path, lines, report)
        if path.endswith(".kt"):
            check_imports(path, lines, report)

    for problem in problems:
        print(problem)
    print("kotlin-lint: %d file(s) checked, %d problem(s)" % (checked, len(problems)))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
