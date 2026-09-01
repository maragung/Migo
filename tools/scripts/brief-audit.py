#!/usr/bin/env python3
"""Consistency gate for migo.md against shared/protocol/schema and docs/.

The brief is normative (see migo.md section 178). A brief that drifts away from
the schema is worse than no brief at all, because people follow it. This script
is the mechanical part of that audit: everything it checks is a fact that can be
verified without reading prose.

Usage: python3 tools/scripts/brief-audit.py [--brief PATH] [--root PATH]
Exit code 0 = clean, 1 = at least one inconsistency.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

SECTION_RE = re.compile(r"^(\d+)\. ([A-Z\"].*)$")
SCREAMING_RE = re.compile(r"\b[A-Z][A-Z0-9]{2,}(?:_[A-Z0-9]+)+\b")
FROZEN_SECTIONS = 135  # docs/*.md cite "brief §NN" for 1..135; never renumber.


class Audit:
    def __init__(self) -> None:
        self.problems: list[str] = []
        self.checks = 0

    def ok(self, label: str) -> None:
        self.checks += 1
        print(f"  ok    {label}")

    def fail(self, label: str, detail: str) -> None:
        self.checks += 1
        self.problems.append(f"{label}: {detail}")
        print(f"  FAIL  {label}\n        {detail}")

    def expect(self, cond: bool, label: str, detail: str) -> bool:
        if cond:
            self.ok(label)
        else:
            self.fail(label, detail)
        return cond


def load_sections(text: str) -> dict[int, tuple[str, str]]:
    """Strict scan: only accept a header whose number is the next expected one.

    The brief contains lists such as "1. Trivia" / "2. RPS" inside sections; a
    lenient regex would mistake those for section headers and silently mis-slice
    the document.
    """
    lines = text.split("\n")
    starts: dict[int, int] = {}
    expected = 0
    for i, line in enumerate(lines):
        m = SECTION_RE.match(line)
        if m and int(m.group(1)) == expected:
            starts[expected] = i
            expected += 1
    out: dict[int, tuple[str, str]] = {}
    keys = sorted(starts)
    for n, start in ((k, starts[k]) for k in keys):
        nxt = starts.get(n + 1, len(lines))
        title = SECTION_RE.match(lines[start]).group(2).strip()
        out[n] = (title, "\n".join(lines[start:nxt]))
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parents[2]))
    ap.add_argument("--brief", default=None)
    args = ap.parse_args()
    root = Path(args.root)
    brief_path = Path(args.brief) if args.brief else root / "migo.md"

    a = Audit()
    text = brief_path.read_text(encoding="utf-8")
    sections = load_sections(text)
    schema = root / "shared" / "protocol" / "schema"

    def js(name: str):
        return json.loads((schema / f"{name}.json").read_text(encoding="utf-8"))

    meta, opcodes, enums, errors = js("meta"), js("opcodes"), js("enums"), js("errors")
    opcode_list = opcodes["opcodes"] if isinstance(opcodes, dict) else opcodes
    enum_list = enums["enums"] if isinstance(enums, dict) else enums
    error_list = errors["errors"] if isinstance(errors, dict) else errors

    try:
        shown = brief_path.resolve().relative_to(root.resolve())
    except ValueError:
        shown = brief_path
    print(f"audit {shown}  ({len(text.splitlines())} lines)\n")
    print("structure")

    # --- structure -----------------------------------------------------------
    nums = sorted(sections)
    a.expect(nums == list(range(len(nums))), "section numbering is gapless from 0",
             f"got {nums[:5]}..{nums[-5:]}")
    a.expect(max(nums) >= FROZEN_SECTIONS, f"sections 1..{FROZEN_SECTIONS} present",
             f"highest section is {max(nums)}")

    # Section 178 promises that every "section NN" reference resolves. It has to
    # be checked here, because a dangling reference reads exactly like a real one:
    # nothing about "lihat section 181" looks wrong until somebody goes looking.
    internal = sorted({int(m) for m in re.findall(r"\bsection (\d+)\b", text)})
    dangling = [r for r in internal if r not in sections]
    a.expect(not dangling, f"every internal section reference resolves ({len(internal)} distinct)",
             f"dangling {dangling}")

    titles: dict[str, list[int]] = {}
    for n, (title, _) in sections.items():
        titles.setdefault(title, []).append(n)
    dups = {t: v for t, v in titles.items() if len(v) > 1}
    a.expect(not dups, "no duplicate section titles", f"{dups}")

    # --- style ---------------------------------------------------------------
    print("\nstyle")
    style = {
        "markdown headings": [i + 1 for i, l in enumerate(text.split("\n")) if l.startswith("#")],
        "bullet lists": [i + 1 for i, l in enumerate(text.split("\n"))
                         if re.match(r"^\s*[-*+]\s+\S", l)],
        "backticks": [i + 1 for i, l in enumerate(text.split("\n")) if "`" in l],
        "bold markers": [i + 1 for i, l in enumerate(text.split("\n")) if "**" in l],
        "trailing whitespace": [i + 1 for i, l in enumerate(text.split("\n"))
                                if l != l.rstrip()],
    }
    for label, hits in style.items():
        a.expect(not hits, f"no {label}", f"lines {hits[:8]}")

    # --- limits --------------------------------------------------------------
    print("\nlimits, flags, features")
    limits = meta["limits"] if isinstance(meta.get("limits"), dict) else {
        l["name"]: l["value"] for l in meta["limits"]}
    missing_named, wrong_value = [], []
    other_limits = {k: str(v) for k, v in limits.items()}
    for name, value in limits.items():
        if name not in text:
            missing_named.append(name)
            continue
        # The first number printed after a limit name must be that limit's value.
        # The window stops at the next limit name so that a sentence naming two
        # limits ("RESUME_BUFFER_FRAMES 512 ... RESUME_WINDOW_MS 120000") is not
        # read as one claim about the first limit.
        for m in re.finditer(re.escape(name) + r"([^\n]{0,90})", text):
            tail = m.group(1)
            cut = min((tail.find(o) for o in other_limits
                       if o != name and tail.find(o) >= 0), default=-1)
            if cut >= 0:
                tail = tail[:cut]
            lits = re.findall(r"\b\d[\d_]*\b", tail)
            if lits and int(lits[0].replace("_", "")) != int(value):
                wrong_value.append((name, lits[0], value))
                break
    a.expect(not missing_named, f"all {len(limits)} schema limits named in the brief",
             f"missing {missing_named}")
    a.expect(not wrong_value, "limit values match meta.json", f"{wrong_value[:5]}")

    flags = meta["flags"]
    active = [f for f in flags if not f["name"].startswith(("RESERVED", "FLAGS_EXT"))]
    missing_flags = [f["name"] for f in active if f["name"] not in text]
    a.expect(not missing_flags, f"all {len(active)} active frame flags named",
             f"missing {missing_flags}")

    features = meta["features"]
    missing_feat = [f["name"] for f in features if f["name"] not in text]
    a.expect(not missing_feat, f"all {len(features)} feature bits named",
             f"missing {missing_feat}")

    # --- opcodes -------------------------------------------------------------
    print("\nopcodes, enums, errors")
    # A name can be both an opcode and a feature bit (TYPING is opcode 39 and
    # feature bit 5), so a bare "5 TYPING" in the feature table is not a wrong
    # opcode number. Accept either reading.
    feature_bit = {f["name"]: f["bit"] for f in features if "bit" in f}
    bad_op = []
    for op in opcode_list:
        name, code = op["name"], op["code"]
        if name not in text:
            bad_op.append((name, "absent"))
            continue
        # wherever the brief prints "NAME = N" or "N NAME", N must be the real code
        for m in re.finditer(r"(?:\b(\d+)\s+" + re.escape(name) + r"\b)|(?:\b"
                             + re.escape(name) + r"\s*=\s*(\d+))", text):
            got = int(m.group(1) or m.group(2))
            if got == code or got == feature_bit.get(name):
                continue
            bad_op.append((name, f"printed as {got}, schema says {code}"))
            break
    a.expect(not bad_op, f"all {len(opcode_list)} schema opcodes named with the right code",
             f"{bad_op[:6]}")

    # No response opcodes: responses ride the request correlation and are structs.
    fake_response_opcodes = [n for n in ("MESSAGE_ACCEPTED", "SYNC_RESPONSE",
                                        "SUBSCRIBE_RESPONSE", "FED_WELCOME",
                                        "CONVERSATION_LIST_RESPONSE", "PONG", "WELCOME")
                             if n in text]
    a.expect(not fake_response_opcodes,
             "responses are not written as SCREAMING_CASE opcodes",
             f"{fake_response_opcodes} should be PascalCase structs")

    missing_enums = [e["name"] for e in enum_list if e["name"] not in text]
    a.expect(not missing_enums,
             f"all {len(enum_list)} schema enums referenced by name",
             f"{len(missing_enums)} unreferenced: {missing_enums}")

    quoted_errors = [e["symbol"] for e in error_list if e["symbol"] in text]
    a.expect(len(quoted_errors) >= 25,
             "error registry quoted verbatim from errors.json",
             f"only {len(quoted_errors)} of {len(error_list)} symbols appear")

    # every SCREAMING_CASE token in the brief must be explainable
    known = set(limits) | {f["name"] for f in flags} | {f["name"] for f in features}
    known |= {o["name"] for o in opcode_list} | {e["symbol"] for e in error_list}
    known |= {c["name"] for c in meta.get("delivery_classes", [])}
    known |= {c["name"] for c in meta.get("auth_levels", [])}
    allow = {
        "MIGO_CONFIG", "RECORD_AUDIO", "TLS_1", "HKDF_SHA256", "MWP_1",
        "VOICE_NOTE_SEND", "VOICE_NOTE_DELETE", "VOICE_NOTE_FORWARD",
        "VOICE_NOTE_PLAY", "CALL_START", "CALL_JOIN", "ROOM_KICK", "ROOM_BAN",
        "ROOM_MUTE", "ROOM_INVITE", "MESSAGE_DELETE_ANY", "MESSAGE_PIN",
        "MEDIA_UPLOAD", "GIFT_SEND", "BOT_MANAGE", "ROOM_SETTINGS",
        "MEMBER_ROLE_SET", "ANNOUNCEMENT_POST", "GAME_START", "USER_AGENT",
        "MAX_PAGE", "MAX_LEDGER_LEGS", "NOT_FOUND", "PERMISSION_DENIED",
        "READ_ONLY", "LOW_DATA", "ULTRA_LOW_DATA", "END_TO_END",
        # Section 184's wallet: the one spec-41 state whose name carries an
        # underscore, and the encrypted vault field that holds the client's
        # own transaction list. Chain-side names, not schema tokens.
        "AWAITING_CONFIRMATION", "FIELD_TXS",
    }
    # Opcodes the brief plans but has not implemented are legitimate, provided
    # they are declared in the section 145 registry under a SPEC marker rather
    # than invented inline somewhere in the prose.
    registry = sections[145][1] if 145 in sections else ""
    a.expect("STATUS: SPEC" in registry and "STATUS: SCHEMA" in registry,
             "section 145 separates SCHEMA opcodes from SPEC opcodes",
             "registry is missing one of the STATUS markers")
    # Sections that are themselves registries may introduce names: 48 declares
    # product permissions, 72 feature bits, 140 frame flags, 145 opcodes, 161
    # error codes. A name used elsewhere in the brief must trace back to one of
    # them (or to the schema), which is what stops ad-hoc invented identifiers.
    declared: set[str] = set()
    for n in (48, 72, 140, 145, 148, 161):
        if n in sections:
            declared |= set(SCREAMING_RE.findall(sections[n][1]))
    unknown: dict[str, int] = {}
    for tok in SCREAMING_RE.findall(text):
        if tok in known or tok in allow or tok in declared:
            continue
        if tok.startswith("MIGO_"):
            continue
        unknown[tok] = unknown.get(tok, 0) + 1
    a.expect(not unknown,
             "every SCREAMING_CASE token traces to the schema or a registry section",
             f"{sorted(unknown)[:10]}")

    # --- cross-document ------------------------------------------------------
    print("\ncross-document")
    docs = root / "docs"
    p02 = (docs / "02-protocol.md").read_text(encoding="utf-8") if (docs / "02-protocol.md").exists() else ""
    p05 = (docs / "05-bandwidth-budget.md").read_text(encoding="utf-8") if (docs / "05-bandwidth-budget.md").exists() else ""

    refs = sorted({int(m) for f in docs.glob("*.md")
                   for m in re.findall(r"brief §(\d+)", f.read_text(encoding="utf-8"))})
    broken = [r for r in refs if r not in sections or r > FROZEN_SECTIONS]
    a.expect(not broken, f'docs "brief §NN" references resolve (found {refs})',
             f"broken {broken}")

    def squash(s: str) -> str:
        return re.sub(r"[\s   ]", "", s)

    if p02:
        frame = str(limits["MAX_FRAME_BYTES"] if not isinstance(limits["MAX_FRAME_BYTES"], dict)
                    else limits["MAX_FRAME_BYTES"]["value"])
        a.expect(frame in squash(p02), "MAX_FRAME_BYTES agrees with docs/02-protocol.md",
                 f"{frame} not found in docs/02-protocol.md")
    if p05:
        metrics = re.findall(r"\bmigo_[a-z_]+\b", p05)
        missing_m = sorted({m for m in metrics if m not in text})
        a.expect(not missing_m, "metric names agree with docs/05-bandwidth-budget.md",
                 f"missing {missing_m}")

    # --- requirement presence ------------------------------------------------
    print("\nrequirements")
    required_topics = {
        "binary-first mandate": ["Binary-First", "WAJIB"],
        "JSON confined to REST/config/admin": ["REST", "configuration"],
        "MessagePack and CBOR rejected": ["MessagePack", "CBOR"],
        "STUN / TURN / SFU / ICE restart": ["STUN", "TURN", "SFU", "ICE restart"],
        "voice note pipeline": ["resumable", "waveform", "offline queue"],
        "federation transport": ["QUIC", "TLS 1.3", "replay"],
        "key storage split": ["Keystore", "IndexedDB", "Web Crypto"],
    }
    for label, needles in required_topics.items():
        miss = [n for n in needles if n not in text]
        a.expect(not miss, label, f"missing {miss}")

    # Section 178 promises these checks are automated; they have to actually run.
    # Each is line-scoped: a forbidden word is fine in a sentence that forbids it.
    PROHIBITION = ("TIDAK BOLEH", "tidak boleh", "Tidak ada", "tidak ada",
                   "bukan", "Jangan", "jangan", "Hindari", "hindari",
                   "dilarang", "Yang tidak boleh", "menghindari")

    # The brief writes prohibitions as a list under a heading ("Yang TIDAK BOLEH:"),
    # so a line's context is the nearest preceding line that ends in a colon. A
    # forbidden word inherits the prohibition from that heading.
    doc_lines = text.split("\n")
    context: list[str] = []
    heading = ""
    for ln in doc_lines:
        if ln.rstrip().endswith(":"):
            heading = ln
        context.append(heading)

    def offending(pattern: str, allowed: tuple[str, ...] = (),
                  flags: int = 0) -> list[str]:
        out = []
        for i, ln in enumerate(doc_lines):
            if not re.search(pattern, ln, flags):
                continue
            scope = ln + " || " + context[i]
            if any(w in scope for w in PROHIBITION) or any(w in scope for w in allowed):
                continue
            out.append(ln[:120])
        return out

    json_ok = ("REST", "public API", "configuration", "config", "admin",
               "debugging", "log", "test fixture", "IDL", "boleh",
               "diperbolehkan", "MWP")
    json_re = r"\bJSON\b"
    json_ok = json_ok + (".json",)
    a.expect(not offending(json_re, json_ok),
             "every JSON mention is a prohibition or an allowed context",
             f"{offending(json_re, json_ok)[:3]}")

    fmt_re = r"MessagePack|CBOR|[Bb]ase64"
    a.expect(not offending(fmt_re),
             "MessagePack, CBOR and base64 appear only as rejections",
             f"{offending(fmt_re)[:3]}")

    poll_re = r"[Pp]olling|setInterval"
    a.expect(not offending(poll_re, ("section",)),
             "polling and setInterval appear only as rejections",
             f"{offending(poll_re, ('section',))[:3]}")

    store_re = r"localStorage|sessionStorage"
    a.expect(not offending(store_re),
             "localStorage and sessionStorage appear only as prohibitions",
             f"{offending(store_re)[:3]}")

    # Every protocol section must declare whether it describes shipped code.
    unmarked = [n for n in range(136, max(nums) + 1)
                if n in sections and "STATUS:" not in sections[n][1]]
    a.expect(not unmarked, "every protocol section carries a STATUS marker",
             f"unmarked {unmarked}")

    # Opcode names used anywhere must be declared in the section 145 registry,
    # not only in the schema — the registry is what a reader consults.
    undeclared = [o["name"] for o in opcode_list
                  if not re.search(r"\b" + re.escape(o["name"]) + r"\b", registry)]
    a.expect(not undeclared, "every schema opcode is listed in the section 145 registry",
             f"missing {undeclared}")

    # Section 177 claims which crates exist. That claim rots the moment somebody
    # adds a workspace member and forgets the list, and it rots silently: a stale
    # BUILT entry reads exactly like a true one. So it is checked against Cargo.toml
    # rather than trusted. Two directions, because both failures happen: a crate
    # claimed but absent, and a crate present but unlisted.
    status = sections.get(177, ("", ""))[1]
    manifest = root / "server" / "Cargo.toml"
    if manifest.exists():
        # Only the members array. [workspace.dependencies] declares a path for every
        # planned crate, so matching the whole file would call all 27 of them real.
        manifest_text = manifest.read_text(encoding="utf-8")
        members_block = re.search(r"members\s*=\s*\[(.*?)\]", manifest_text, re.S)
        members = re.findall(r'"crates/([a-z0-9-]+)"',
                             members_block.group(1) if members_block else "")
        # Three blocks, not two. A crate that compiles cleanly but has no test may not
        # be called BUILT — the maintenance rule at the end of the section says so — and
        # may not be left in BELUM ADA KODE either, or somebody rewrites code that
        # already exists. So there is a middle block, and the split has to respect it:
        # matching the crate name anywhere before "SCHEMA, sudah di IDL" would count a
        # middle-block entry as a BUILT one, which is the exact claim the rule forbids.
        untested_header = "KODE LENGKAP, TEST BELUM DITULIS:"
        built_block = status.split(untested_header)[0].split("SCHEMA, sudah di IDL")[0]
        untested_block = (status.split(untested_header)[-1].split("SCHEMA, sudah di IDL")[0]
                          if untested_header in status else "")
        no_code_block = status.split("BELUM ADA KODE:")[-1]

        a.expect(bool(members), "server/Cargo.toml declares workspace members",
                 "the members array could not be parsed")

        # An entry, not a mention. Every status line in this section begins with the
        # thing it describes, and the descriptions are long enough to name other crates
        # in passing — the moderation entry explains how it differs from migo-auth and
        # why its port trait has the same shape as one in migo-media. Matching the name
        # anywhere in the block read both of those as status entries, which put
        # migo-auth in two blocks at once and would have made this check unusable
        # precisely as the descriptions got more useful.
        def listed_in(block, crate):
            return bool(re.search(r"(?m)^" + re.escape(crate) + r"\b", block))

        def mentioned_in(block, crate):
            return bool(re.search(r"\b" + re.escape(crate) + r"\b", block))

        # Every member must be accounted for by one of the two blocks that claim code
        # exists. Which of the two is checked separately below.
        unaccounted = sorted(c for c in members
                             if not listed_in(built_block, c)
                             and not listed_in(untested_block, c))
        a.expect(not unaccounted,
                 f"every workspace member has a status entry in section 177 ({len(members)} crates)",
                 f"a member of server/Cargo.toml with no BUILT and no untested entry: "
                 f"{unaccounted}")

        # The failure this exists for: a status update that adds the new BUILT line
        # and forgets to delete the old BELUM ADA KODE one, leaving both true-looking.
        contradictory = sorted(c for c in members
                               if mentioned_in(no_code_block, c))
        a.expect(not contradictory,
                 "no crate with code is listed as BELUM ADA KODE",
                 f"{contradictory}")

        # A crate cannot be in both of the code-exists blocks: one says it has passing
        # tests and the other says it has none, and a reader who finds it in the first
        # will never look for it in the second.
        doubled = sorted(c for c in members
                         if listed_in(built_block, c) and listed_in(untested_block, c))
        a.expect(not doubled,
                 "no crate is listed as both BUILT and untested",
                 f"{doubled}")

        # The point of the middle block: a crate listed there must actually have no
        # tests. The moment a test appears, the entry is stale in the direction that
        # flatters the project, which is the direction nobody notices.
        #
        # Both places a Rust test can live, not just one. The first version of this
        # check globbed crates/<name>/tests/*.rs and nothing else, so a crate whose
        # tests were unit tests in a #[cfg(test)] module -- which is where most of this
        # workspace's tests are -- kept its untested entry and this gate kept reporting
        # green. A checker that can only see half the places a thing hides is worse
        # than no checker, because it is the reason nobody looks in the other half.
        def has_tests(crate):
            crate_dir = root / "server" / "crates" / crate
            if any((crate_dir / "tests").glob("*.rs")):
                return True
            for source in (crate_dir / "src").rglob("*.rs"):
                body = source.read_text(encoding="utf-8")
                if re.search(r"(?m)^\s*#\[(?:tokio::)?test\]", body):
                    return True
            return False

        premature = sorted(c for c in members if listed_in(untested_block, c) and has_tests(c))
        a.expect(not premature,
                 "no crate listed as untested actually has tests",
                 f"tests exist, so move these to BUILT: {premature}")

    # Public/Managed Room must never be described as end-to-end encrypted.
    room_e2e = [ln for ln in text.split("\n")
                if re.search(r"(Public Room|Managed Room)", ln)
                and re.search(r"end-to-end|end to end|E2E", ln)
                and not re.search(r"tidak|bukan|luar scope|luar lingkup|out of scope", ln, re.I)]
    a.expect(not room_e2e, "no unqualified E2E claim for Public/Managed Room",
             f"{room_e2e[:2]}")

    print(f"\n{a.checks} checks, {len(a.problems)} problem(s)")
    if a.problems:
        print("\nPROBLEMS")
        for p in a.problems:
            print(f"  - {p}")
        return 1
    print("clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
