#!/usr/bin/env python3
"""Static checks on the Android client's Kotlin, for a sandbox with no JVM.

Nothing in this repository can compile Kotlin locally: the module needs a JDK, the
Android SDK and Gradle, and .github/workflows/android.yml is the only place all three
exist. Every mistake in this tree therefore costs a push and a runner, and the two
mistakes this script was written for cost one each.

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

2. No `vararg` of an inline class. The JVM has no array type for a `@JvmInline value
   class`, so `vararg ids: Id` is rejected outright -- but only by the compiler, and only
   after Gradle has resolved the Android SDK. This one cost the runner immediately after
   the comment bug was fixed, which is the pattern this script exists to break: one round
   trip per class of mistake, not one per mistake.

3. Cyrillic in source. U+0400-U+04FF contains a/e/o/c/p/x glyphs identical to the Latin
   ones, so a copy-paste can silently produce a different identifier that reads right and
   resolves to nothing. Nothing in review catches it.

4. Imports: unused, duplicated, or out of order. The compiler warns about unused imports
   but android.yml does not fail on warnings (deliberately -- see its header), so they
   accumulate unnoticed, and there is no ktlint or detekt to sort them.

5. Trailing whitespace, because it is free to check.

Every check except the comment one runs against a copy of the file with comment and
string-literal *content* blanked out and newlines kept, so line numbers still line up.
That is not a nicety: the first version of check 2 flagged the KDoc sentence explaining
why the vararg was removed. A gate that reports its own documentation is a gate people
switch off.

Line length is NOT checked: protocol/Generated.kt is generator output whose lines are
long by construction, and a gate that demands edits to generated files is a gate someone
will delete.

Exit status is 1 if anything is reported, so this can be a Makefile gate.

Run with `--selftest` to check the checker: it writes each known-bad shape and each
look-alike-but-harmless shape to a temporary directory and asserts which ones get
reported. A checker nothing has ever proved wrong proves nothing, and this one has no
compiler standing behind it to notice if a regex quietly stops matching.
"""

import os
import re
import shutil
import sys
import tempfile

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

# `@JvmInline` may sit on its own line above the declaration or share it, and the class
# may be `internal`, `private` or carry further annotations, so the gap is loose.
INLINE_CLASS = re.compile(r"@JvmInline[\s\S]{0,200}?\bvalue\s+class\s+(\w+)")

VARARG = re.compile(r"\bvararg\s+\w+\s*:\s*([\w.]+)")

KDOC_LINK = re.compile(r"\[([\w.]+)")

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


def scan(src):
    """Splits a file into code, comment text, and the lines of unclosed block comments.

    Returns `(code, comments, unclosed)`. `code` is the same length as `src` with every
    comment and string-literal character replaced by a space and every newline kept, so
    an offset or a line number means the same thing in both. `comments` is the comment
    text alone, for the KDoc-link scan. `unclosed` lists the line each still-open block
    comment started on, which is check 1's whole output.

    One scanner rather than one per check, because "is this position inside a comment"
    is the question every check here has to get right, and three implementations of it
    would disagree.
    """
    code = []
    comments = []
    opens = []
    line = 1
    i = 0
    n = len(src)

    def blank(text):
        code.append("".join(c if c == "\n" else " " for c in text))

    while i < n:
        char = src[i]
        if char == "\n":
            code.append("\n")
            line += 1
            i += 1
        elif opens:
            if src.startswith("/*", i):
                opens.append(line)
                blank(src[i : i + 2])
                i += 2
            elif src.startswith("*/", i):
                opens.pop()
                blank(src[i : i + 2])
                i += 2
            else:
                comments.append(char)
                blank(char)
                i += 1
        elif src.startswith("//", i):
            end = src.find("\n", i)
            end = n if end < 0 else end
            comments.append(src[i:end])
            comments.append("\n")
            blank(src[i:end])
            i = end
        elif src.startswith("/*", i):
            opens.append(line)
            blank(src[i : i + 2])
            i += 2
        elif src.startswith('"""', i):
            end = src.find('"""', i + 3)
            end = n if end < 0 else end + 3
            line += src.count("\n", i, end)
            blank(src[i:end])
            i = end
        elif char in "\"'":
            start = i
            i += 1
            while i < n and src[i] != char:
                if src[i] == "\\":
                    i += 1
                elif src[i] == "\n":
                    line += 1
                i += 1
            i = min(i + 1, n)
            blank(src[start:i])
        else:
            code.append(char)
            i += 1

    return "".join(code), "".join(comments), opens


def check_comments(path, unclosed, report):
    for opened in unclosed:
        report(
            path,
            opened,
            "block comment is never closed. Kotlin block comments nest, so a slash-star "
            "sequence in prose (a glob, a package name) opens one",
        )


def check_text(path, lines, report):
    """Trailing whitespace and Cyrillic. Both are checked on the raw text on purpose.

    A homoglyph in a comment is still worth reporting -- it means a paste came from
    somewhere unexpected -- and trailing whitespace has nothing to do with syntax.
    """
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


def check_varargs(path, code_lines, inline_classes, report):
    """Reports `vararg x: T` where T is an inline class.

    `inline_classes` is collected across the whole tree first, because the declaration is
    almost never in the file that misuses it.
    """
    for number, text in enumerate(code_lines, start=1):
        for match in VARARG.finditer(text):
            named = match.group(1).rsplit(".", 1)[-1]
            if named in inline_classes:
                report(
                    path,
                    number,
                    "vararg of inline class %s does not compile: the JVM has no array "
                    "type for it. Use one overload per arity" % named,
                )


def check_imports(path, lines, code, comments, report):
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

    # An import may not vouch for itself, so its own line is blanked before the search.
    # A KDoc `[Symbol]` reference IS a use of the import, though -- a doc link to a type
    # this file never mentions in code still fails to resolve without it -- so those come
    # from the comment text rather than the code.
    numbers = {number for number, _n, _a in imports}
    body = "".join(t for n, t in enumerate(code.splitlines(keepends=True), start=1)
                   if n not in numbers)
    linked = {ref.split(".")[0] for ref in KDOC_LINK.findall(comments)}

    for number, name, alias in imports:
        if name in DELEGATE_IMPORTS or name.endswith(".*"):
            continue
        symbol = alias or name.rsplit(".", 1)[-1]
        if re.search(r"\b%s\b" % re.escape(symbol), body) or symbol in linked:
            continue
        report(path, number, "unused import %s" % name)


# Each case is (name, source, expected substring of the report, or None for "silent").
# The harmless shapes matter as much as the broken ones: every false positive here landed
# in the tree once, and a gate that cries wolf is a gate someone deletes rather than fixes.
SELFTEST_CASES = (
    (
        "glob in a doc comment",
        "/**\n * Reads `crypto/*.json` from disk.\n */\nclass Probe\n",
        "block comment is never closed",
    ),
    (
        "glob in a line comment is inert",
        "// reads crypto/*.json from disk\nclass Probe\n",
        None,
    ),
    (
        "a comment pair inside a raw string is inert",
        'val raw = """a /* b */ c"""\n',
        None,
    ),
    (
        "vararg of an inline class",
        "@JvmInline\nvalue class Id(val value: String)\n\nfun name(vararg ids: Id) = ids\n",
        "vararg of inline class Id",
    ),
    (
        "prose about a vararg of an inline class is not code",
        "@JvmInline\nvalue class Id(val value: String)\n\n"
        "/** Not `vararg ids: Id`, because that does not compile. */\nfun name(id: Id) = id\n",
        None,
    ),
    (
        "vararg of an ordinary type",
        "fun join(vararg parts: ByteArray) = parts\n",
        None,
    ),
    (
        "unused import",
        "import kotlin.random.Random\n\nclass Probe\n",
        "unused import kotlin.random.Random",
    ),
    (
        "an import used only by a doc link is used",
        "import kotlin.random.Random\n\n/** See [Random]. */\nclass Probe\n",
        None,
    ),
    (
        "an import named only in a string is not used",
        'import kotlin.random.Random\n\nval s = "Random"\n',
        "unused import kotlin.random.Random",
    ),
    (
        "duplicate import",
        "import kotlin.random.Random\nimport kotlin.random.Random\n\nval r = Random\n",
        "duplicate import",
    ),
    (
        "unsorted imports",
        "import kotlinx.coroutines.Job\nimport kotlin.random.Random\n\nval x = listOf(Job, Random)\n",
        "imports are not sorted",
    ),
    (
        "a Compose delegate import is not unused",
        "import androidx.compose.runtime.getValue\n\nclass Probe\n",
        None,
    ),
    (
        "Cyrillic homoglyph",
        "val \u0441ount = 1\n",
        "Cyrillic U+0441",
    ),
    (
        "trailing whitespace",
        "class Probe \n",
        "trailing whitespace",
    ),
)


def selftest():
    """Writes each case to a temp file and checks that the report matches expectation."""
    failures = []
    workspace = tempfile.mkdtemp(prefix="kotlin-lint-selftest-")
    try:
        for index, (name, source, expected) in enumerate(SELFTEST_CASES):
            path = os.path.join(workspace, "Case%d.kt" % index)
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(source)
            reported = []
            main_argv = ["kotlin-lint", path]
            status = _run(main_argv, reported.append)
            joined = " | ".join(reported)
            if expected is None:
                if status != 0:
                    failures.append("%s: expected silence, got: %s" % (name, joined))
            elif expected not in joined:
                failures.append("%s: expected %r, got: %s" % (name, expected, joined or "silence"))
    finally:
        shutil.rmtree(workspace, ignore_errors=True)

    for failure in failures:
        print("selftest FAIL  " + failure)
    print("kotlin-lint selftest: %d case(s), %d failure(s)"
          % (len(SELFTEST_CASES), len(failures)))
    return 1 if failures else 0


def _run(argv, emit):
    """The scan itself, with reporting routed through `emit` so selftest can capture it."""
    roots = argv[1:]
    problems = []

    def report(path, line, message):
        problems.append("%s:%d %s" % (path, line, message))

    # Two passes. An inline class is declared in one file and misused in another, so the
    # set of them has to be complete before any file can be judged.
    scanned = {}
    for path in kotlin_files(roots):
        with open(path, encoding="utf-8") as handle:
            src = handle.read()
        scanned[path] = (src, scan(src))
    inline_classes = {
        name for _src, (code, _c, _u) in scanned.values() for name in INLINE_CLASS.findall(code)
    }

    for path, (src, (code, comments, unclosed)) in scanned.items():
        check_comments(path, unclosed, report)
        check_text(path, src.splitlines(keepends=True), report)
        check_varargs(path, code.splitlines(keepends=True), inline_classes, report)
        if path.endswith(".kt"):
            check_imports(path, src.splitlines(keepends=True), code, comments, report)

    for problem in problems:
        emit(problem)
    return 1 if problems else 0


def main(argv):
    if "--selftest" in argv:
        return selftest()
    roots = argv[1:] or ["clients/android"]
    problems = []
    status = _run(["kotlin-lint"] + roots, problems.append)
    for problem in problems:
        print(problem)
    print("kotlin-lint: %d problem(s)" % len(problems))
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv))
