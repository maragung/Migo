#!/usr/bin/env python3
"""Consistency gate for migo.md against shared/protocol/schema and docs/.

The brief is normative (see migo.md section 178). A brief that drifts away from
the schema is worse than no brief at all, because people follow it. This script
is the mechanical part of that audit: everything it checks is a fact that can be
verified without reading prose.

The cross-document half of section 178's list lives here too, and it is honest
about its own reach. Names and numbers can be compared mechanically — limits,
feature bits, opcode registry entries, backoff and heartbeat figures, budget
tables, and whether every opcode, error symbol, enum and permission a product
section relies on actually exists in the protocol section it cites. What no
script can do is decide whether the prose of section 179 or section 180 still
means what section 167 or section 165 means; that comparison stays human, and
the checks below say so where they stop.

Usage: python3 tools/scripts/brief-audit.py [--brief PATH] [--root PATH]
       python3 tools/scripts/brief-audit.py --selftest
Exit code 0 = clean, 1 = at least one inconsistency.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
import tempfile
from pathlib import Path

SECTION_RE = re.compile(r"^(\d+)\. ([A-Z\"].*)$")
SCREAMING_RE = re.compile(r"\b[A-Z][A-Z0-9]{2,}(?:_[A-Z0-9]+)+\b")
FROZEN_SECTIONS = 135  # docs/*.md cite "brief §NN" for 1..135; never renumber.


class Audit:
    def __init__(self, quiet: bool = False) -> None:
        self.problems: list[str] = []
        self.checks = 0
        self.quiet = quiet

    def ok(self, label: str) -> None:
        self.checks += 1
        if not self.quiet:
            print(f"  ok    {label}")

    def fail(self, label: str, detail: str) -> None:
        self.checks += 1
        self.problems.append(f"{label}: {detail}")
        if not self.quiet:
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


def run_audit(root: Path, brief_path: Path, quiet: bool = False) -> Audit:
    a = Audit(quiet)

    def say(msg: str) -> None:
        if not quiet:
            print(msg)

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
    say(f"audit {shown}  ({len(text.splitlines())} lines)\n")
    say("structure")

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
    say("\nstyle")
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
    say("\nlimits, flags, features")
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
    say("\nopcodes, enums, errors")
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
    say("\ncross-document")
    docs = root / "docs"
    p02 = (docs / "02-protocol.md").read_text(encoding="utf-8") if (docs / "02-protocol.md").exists() else ""
    p05 = (docs / "05-bandwidth-budget.md").read_text(encoding="utf-8") if (docs / "05-bandwidth-budget.md").exists() else ""

    # Recursive on purpose: docs/adr, docs/design and docs/runbooks carry no
    # "brief §NN" citations today, which is exactly why a top-level glob would
    # be the wrong shape -- the first citation someone adds under a
    # subdirectory would be invisible to the gate, and invisible drift is the
    # only kind this script exists to prevent.
    refs = sorted({int(m) for f in docs.rglob("*.md")
                   for m in re.findall(r"brief §(\d+)", f.read_text(encoding="utf-8"))})
    broken = [r for r in refs if r not in sections or r > FROZEN_SECTIONS]
    a.expect(not broken, f'docs "brief §NN" references resolve (found {len(refs)} across docs/)',
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

    # --- cross-document: brief vs schema (section 178, items 1-3) ------------
    say("\ncross-document: brief vs schema")

    # Item 1, the other direction. "limit values match meta.json" above rejects
    # a wrong number printed beside a name; this rejects the subtler rot, a
    # limit that survives in the prose while its value quietly disappears,
    # leaving a claim the reader cannot check against anything.
    unprinted = []
    for name, value in limits.items():
        printed = False
        for m in re.finditer(re.escape(name) + r"([^\n]{0,90})", text):
            tail = m.group(1)
            cut = min((tail.find(o) for o in other_limits
                       if o != name and tail.find(o) >= 0), default=-1)
            if cut >= 0:
                tail = tail[:cut]
            lits = re.findall(r"\b\d[\d_]*\b", tail)
            if lits and int(lits[0].replace("_", "")) == int(value):
                printed = True
                break
        if not printed:
            unprinted.append(name)
    a.expect(not unprinted,
             f"each of the {len(limits)} limit values is printed beside its name",
             f"no mention prints the schema value: {unprinted}")

    # Item 2. Section 72 calls itself the one binding list, so it is compared
    # entry by entry against meta.json -- a bit that drifted, a name that only
    # exists on one side, and a bit reassigned to a second name are all
    # failures, not just a missing mention.
    bits: dict[str, int] = {}
    for ln in (sections.get(72, ("", ""))[1]).split("\n"):
        m = re.match(r"^(\d+) ([A-Z][A-Z0-9_]+)$", ln.strip())
        if m:
            bits[m.group(2)] = int(m.group(1))
    meta_bits = {f["name"]: f["bit"] for f in features}
    only_bits = sorted(set(bits) - set(meta_bits))
    only_meta = sorted(set(meta_bits) - set(bits))
    bit_drift = sorted((n, b, meta_bits[n]) for n, b in bits.items()
                       if n in meta_bits and meta_bits[n] != b)
    a.expect(bits and not only_bits and not only_meta and not bit_drift,
             f"section 72 feature-bit list matches meta.json ({len(meta_bits)} bits)",
             f"only in section 72: {only_bits}; only in meta.json: {only_meta}; "
             f"wrong bit: {bit_drift}")

    # Item 3. Every "N NAME, ..." line in the section 145 registry -- the
    # SCHEMA block and the planned block alike, because the planned opcodes
    # were implemented and added to opcodes.json as their handlers landed, so
    # the whole registry must agree with the file in both directions and by
    # number. A registry line is an entry by its shape ("N NAME, arah, auth,
    # cost, class"), which the surrounding prose never imitates.
    registry_lines: dict[str, int] = {}
    for ln in (sections.get(145, ("", ""))[1]).split("\n"):
        m = re.match(r"^(\d+) ([A-Z][A-Z0-9_]+), ", ln)
        if m:
            registry_lines[m.group(2)] = int(m.group(1))
    schema_ops = {o["name"]: o["code"] for o in opcode_list}
    only_reg = sorted(set(registry_lines) - set(schema_ops))
    only_schema = sorted(set(schema_ops) - set(registry_lines))
    wrong_code = sorted((n, c, schema_ops[n]) for n, c in registry_lines.items()
                        if n in schema_ops and schema_ops[n] != c)
    a.expect(registry_lines and not only_reg and not only_schema and not wrong_code,
             f"section 145 opcode registry matches opcodes.json ({len(schema_ops)} opcodes)",
             f"only in registry: {only_reg}; only in opcodes.json: {only_schema}; "
             f"wrong code: {wrong_code}")

    # --- cross-document: brief vs docs (section 178, items 4-5) --------------
    say("\ncross-document: brief vs docs")

    if p02:
        # docs/02 wraps lines mid-sentence ("Missing\n  2 intervals"), so every
        # extraction below runs on whitespace-collapsed text.
        p02n = re.sub(r"\s+", " ", p02)

        # The hard-limits table is the one place docs/02 restates meta.json
        # numbers in bulk; a limit changed in the schema but not here (or vice
        # versa) leaves two documents that both look normative.
        hard = {name: int(re.sub(r"\D", "", val)) for name, val in
                re.findall(r"^\|\s*`([A-Z_]+)`\s*\|\s*([\d\s]+?)\s*\|", p02, re.M)}
        unknown_hard = sorted(n for n in hard if n not in limits)
        bad_hard = sorted((n, v, limits[n]) for n, v in hard.items()
                          if n in limits and limits[n] != v)
        a.expect(hard and not unknown_hard and not bad_hard,
                 "docs/02-protocol.md hard-limit table matches meta.json",
                 f"unknown limits: {unknown_hard}; wrong values: {bad_hard}")

        comp = re.search(r"COMPRESS_MIN_BYTES` \((\d+)\)", p02n)
        gain = re.search(r"at least (\d+) % smaller", p02n)
        comp_ok = bool(comp) and int(comp.group(1)) == limits["COMPRESS_MIN_BYTES"]
        gain_ok = bool(gain) and int(gain.group(1)) == limits["COMPRESS_MIN_GAIN_PERCENT"]
        a.expect(comp_ok and gain_ok,
                 "docs/02-protocol.md compression thresholds match meta.json",
                 f"COMPRESS_MIN_BYTES printed as {comp.group(1) if comp else None} "
                 f"(schema {limits['COMPRESS_MIN_BYTES']}), gain printed as "
                 f"{gain.group(1) if gain else None} "
                 f"(schema {limits['COMPRESS_MIN_GAIN_PERCENT']})")

        linger = re.search(r"≤\s*(\d+)\s*ms\**\s*linger", p02n)
        a.expect(bool(linger) and int(linger.group(1)) == limits["BATCH_LINGER_MS"],
                 "docs/02-protocol.md batch linger matches meta.json",
                 f"printed as {linger.group(1) if linger else None} ms "
                 f"(schema {limits['BATCH_LINGER_MS']})")

        # Heartbeat and reconnect, the two timing rules a client author reads
        # from docs/02 first. The resume window is the third timing rule in
        # section 178's list, but docs/02 deliberately states no numbers for it
        # ("a small ring buffer ... for a short window"), so there is nothing to
        # compare there; the brief's RESUME_BUFFER_FRAMES and RESUME_WINDOW_MS
        # values are held against meta.json by the limit checks above.
        backoff_doc = re.search(r"backoff `([\d,]+) s`", p02n)
        doc_seq = [int(x) for x in backoff_doc.group(1).split(",")] if backoff_doc else []
        s18 = sections.get(18, ("", ""))[1]
        brief_seq = [int(l.strip().rstrip("s")) for l in s18.split("\n")
                     if re.match(r"^\d+s$", l.strip())]
        a.expect(bool(doc_seq) and doc_seq == brief_seq,
                 "docs/02-protocol.md reconnect backoff matches section 18",
                 f"docs say {doc_seq}, section 18 says {brief_seq}")

        numerals = {"satu": 1, "dua": 2, "tiga": 3, "empat": 4, "lima": 5, "enam": 6,
                    "tujuh": 7, "delapan": 8, "sembilan": 9, "sepuluh": 10}
        miss_doc = re.search(r"Missing (\d+) intervals", p02n)
        miss_brief = re.search(r"Melewatkan (\w+) interval", s18)
        brief_miss = numerals.get(miss_brief.group(1)) if miss_brief else None
        a.expect(bool(miss_doc) and brief_miss is not None
                 and int(miss_doc.group(1)) == brief_miss,
                 "docs/02-protocol.md heartbeat miss rule matches section 18",
                 f"docs close after {miss_doc.group(1) if miss_doc else '?'} missed intervals, "
                 f"section 18 says {miss_brief.group(1) if miss_brief else '?'} ({brief_miss})")

    if p05:
        # The budget tables (per-event and per-session, everything before the
        # rules section) are compared with section 56 in both directions. The
        # comparison is by number, not by row, because the row labels are
        # English prose in docs/05 and Indonesian prose in the brief; pairing
        # them would mean translating, and a translated pairing is exactly the
        # kind of check that silently stops matching. A number that appears in
        # one document's budgets and not the other's is still always a drift.
        budget_tables = p05.split("## 3.")[0]
        doc_nums: set[int] = set()
        for row in budget_tables.split("\n"):
            stripped = row.strip()
            if not stripped.startswith("|") or set(stripped) <= {"|", "-", " "}:
                continue
            cols = [c.strip() for c in stripped.strip("|").split("|")]
            if len(cols) < 2 or not cols[0] or cols[1] == "Budget":
                continue
            doc_nums |= {int(n.replace(",", "")) for n in
                         re.findall(r"(\d[\d,]*)\s*(?:B|KB|s|%)\b", cols[1])}
        s56 = sections.get(56, ("", ""))[1]
        brief_max = {int(m.group(1)) for m in re.finditer(r"Maksimum (\d+) (?:byte|KB)", s56)}
        fwd = sorted(n for n in doc_nums if str(n) not in s56)
        rev = sorted(n for n in brief_max if n not in doc_nums)
        a.expect(doc_nums and not fwd and not rev,
                 "docs/05-bandwidth-budget.md budget numbers match section 56",
                 f"docs numbers absent from section 56: {fwd}; "
                 f"section 56 budgets absent from docs/05: {rev}")

    # --- product sections vs protocol sections (section 178, items 6-7) ------
    # What follows is the mechanical share of "requirement produk WAJIB
    # konsisten dengan protokolnya": every identifier and number a product
    # section relies on must exist where the product section says it lives.
    # Whether the prose still means the same thing is deliberately not checked
    # here -- that is the part of items 6 and 7 that stays with a human reader,
    # because a script can compare names and numbers, not intent.
    say("\nproduct requirements vs protocol")
    s48 = sections.get(48, ("", ""))[1]
    s165 = sections.get(165, ("", ""))[1]
    s166 = sections.get(166, ("", ""))[1]
    s167 = sections.get(167, ("", ""))[1]
    s168 = sections.get(168, ("", ""))[1]
    s179 = sections.get(179, ("", ""))[1]
    s180 = sections.get(180, ("", ""))[1]
    s179n = re.sub(r"\s+", " ", s179)
    s180n = re.sub(r"\s+", " ", s180)
    error_syms = {e["symbol"] for e in error_list}

    # Section 180 names the feature bits that gate calls. Section 72 records
    # that the bit names were renamed once already (CALL_V1 became CALLS), so a
    # product section resurrecting a dead name is not hypothetical drift.
    feat_names = {f["name"] for f in features}
    m = re.search(r"Feature bit yang mengatur ketersediaannya adalah ([^.]+)\.", s180n)
    claimed = SCREAMING_RE.findall(m.group(1)) if m else []
    unknown_feat = [t for t in claimed if t not in feat_names]
    a.expect(m and claimed and not unknown_feat,
             "section 180 feature-bit claims are real bits in meta.json",
             f"claimed {claimed or 'nothing'}; not in meta.json: "
             f"{unknown_feat or 'no claim found'}")

    call_toks = sorted({t for t in SCREAMING_RE.findall(s180) if t.startswith("CALL_")})
    ungrounded_call = [t for t in call_toks if t not in s165 and t not in s166]
    a.expect(bool(call_toks) and not ungrounded_call,
             "section 180 call identifiers appear in sections 165 and 166",
             f"{ungrounded_call} named in the product requirement but absent from the protocol")

    # The promises "dijawab X" / "ditolak dengan X" name error symbols the
    # server is said to answer with; those must be real registry symbols,
    # because a client author will switch on them.
    answer_tokens: set[str] = set()
    for sent in re.split(r"[.\n]", s179 + "\n" + s180):
        if "dijawab" in sent or "ditolak" in sent:
            answer_tokens |= set(SCREAMING_RE.findall(sent))
    bad_answers = sorted(t for t in answer_tokens if t not in error_syms)
    a.expect(not bad_answers,
             "error symbols promised in sections 179 and 180 exist in errors.json",
             f"{bad_answers}")

    def participants(s: str):
        mm = re.search(r"(\d+) peserta audio[^.]*?(\d+) stream video", re.sub(r"\s+", " ", s))
        return (int(mm.group(1)), int(mm.group(2))) if mm else None

    a.expect(participants(s165) is not None and participants(s165) == participants(s180),
             "section 180 participant limits match section 165",
             f"section 165 says {participants(s165)}, "
             f"section 180 says {participants(s180)}")

    # Wire states must be a subset, not equal: the product section adds
    # Degraded, which a client derives locally from call quality and which
    # never rides CallStateEvent.
    m165 = re.search(r"state yaitu ([A-Za-z, ]+?), ditambah", re.sub(r"\s+", " ", s165))
    wire_states = set()
    if m165:
        for w in m165.group(1).split(","):
            w = w.strip()
            if w.startswith("atau "):
                w = w[5:].strip()
            if w:
                wire_states.add(w)
    m180 = re.search(r"keadaan dan client WAJIB menampilkannya[^\n]*\n\n(.+?)(?:\n\n|$)",
                     s180, re.S)
    ui_states = ({ln.split(",")[0].strip() for ln in m180.group(1).split("\n") if ln.strip()}
                 if m180 else set())
    a.expect(bool(wire_states) and bool(ui_states) and wire_states <= ui_states,
             "section 165 wire call states appear in the section 180 state list",
             f"wire states {sorted(wire_states)}; section 180 lists {sorted(ui_states)}")

    media_toks = sorted({t for t in SCREAMING_RE.findall(s179) if t.startswith("MEDIA_")})
    ungrounded_media = [t for t in media_toks if t not in s167 and t not in s168]
    a.expect(bool(media_toks) and not ungrounded_media,
             "section 179 media opcodes appear in sections 167 and 168",
             f"{ungrounded_media} named in the product requirement but absent from the protocol")

    variant_map: dict[str, set[str]] = {}
    for e in enum_list:
        vs = e.get("variants", e.get("values"))
        variant_map[e["name"]] = {v["name"] if isinstance(v, dict) else v for v in vs}
    named_enums = set(re.findall(r"enum ([A-Z]\w+)", s179n))
    unknown_enums = sorted(n for n in named_enums if n not in variant_map)
    a.expect(bool(named_enums) and not unknown_enums,
             "enums named in section 179 exist in enums.json",
             f"{unknown_enums}")

    value_claims = [(e, v) for e, v in re.findall(r"([A-Z]\w+) bernilai (\w+)", s179n)]
    value_claims += [(e, v) for v, e in
                     re.findall(r"\w+ bernilai (\w+) dari enum ([A-Z]\w+)", s179n)]
    bad_values = sorted({(e, v) for e, v in value_claims
                         if e in variant_map and v not in variant_map[e]})
    a.expect(not bad_values,
             "enum values claimed in section 179 are real variants",
             f"{bad_values}")

    # Section 167 offers 1x, 1.5x and 2x; section 179 offers those plus 0.5x.
    # The protocol's offer must fit inside the product's -- the product
    # promising a speed the protocol never mentions is the drift that matters.
    sp167 = set(re.findall(r"\d+(?:\.\d+)?x",
                           next(l for l in s167.split("\n") if "Playback speed" in l)))
    sp179 = set(re.findall(r"\d+(?:\.\d+)?x", " ".join(
        ln for ln in s179.split("\n") if "Kecepatan" in ln and "x" in ln)))
    a.expect(bool(sp167) and sp167 <= sp179,
             "section 167 playback speeds appear in section 179",
             f"section 167 offers {sorted(sp167)}, section 179 offers {sorted(sp179)}")

    mperm = re.search(r"melalui permission pada section 48, yaitu ([^.]+)\.", s179n)
    perms = SCREAMING_RE.findall(mperm.group(1)) if mperm else []
    not_in_48 = [p2 for p2 in perms if p2 not in s48]
    a.expect(mperm and perms and not not_in_48,
             "voice note room permissions named in section 179 exist in section 48",
             f"{not_in_48 or 'no permission list found'}")

    # --- requirement presence ------------------------------------------------
    say("\nrequirements")
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

    # The four status words are the whole vocabulary, and a fifth one is not a
    # stylistic slip: every reader of this document, human or script, decides what
    # to do with a section by matching on that word, so "STATUS: BUILD" reads as a
    # status nobody defined -- is it weaker than BUILT, or the same thing spelled
    # wrong? -- and only the person who wrote it knows. This is the one status
    # defect no other check here can see: the checks around it ask whether a BUILT
    # claim has code behind it, and none of them has an opinion about a word that
    # claims nothing.
    STATUS_WORDS = ("BUILT", "SCHEMA", "SPEC", "SEBAGIAN")
    bogus = sorted({w for n in sections
                    for w in re.findall(r"STATUS: ([A-Z]+)", sections[n][1])
                    if w not in STATUS_WORDS})
    a.expect(not bogus, "every STATUS marker uses one of the document's four status words",
             f"{bogus[:6]}")

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

    # The waveform bucket count is one number everywhere it is stated. The
    # brief pins it (section 167) and all three clients encode it as a
    # constant — but the count rides inside E2E ciphertext, where no schema
    # or vector can check it, so a client drifting from the brief is silent
    # on every existing gate. Section 171's 64 is a ceiling, not the count,
    # and is deliberately not pinned here.
    waveform_sites = {
        "web": root / "clients/web/src/lib/migo/voice.ts",
        "android": root / "clients/android/app/src/main/kotlin/com/migo/app/media/Media.kt",
        "desktop": root / "clients/desktop/src/net/media.rs",
    }
    waveform_counts = {}
    for name, path in waveform_sites.items():
        if path.exists():
            found = re.search(r"WAVEFORM_BARS[^=\n]*=\s*(\d+)",
                              path.read_text(encoding="utf-8"))
            if found:
                waveform_counts[name] = int(found.group(1))
    brief_167 = re.search(r"jumlah bucket tetap, yaitu (\d+) bucket",
                          sections.get(167, ("", ""))[1])
    brief_count = int(brief_167.group(1)) if brief_167 else None
    a.expect(brief_count is not None and len(waveform_counts) == len(waveform_sites)
             and all(c == brief_count for c in waveform_counts.values()),
             "the waveform bucket count is one number in the brief and all three clients",
             f"brief section 167 says {brief_count}; clients say {waveform_counts}")

    # Public/Managed Room must never be described as end-to-end encrypted.
    room_e2e = [ln for ln in text.split("\n")
                if re.search(r"(Public Room|Managed Room)", ln)
                and re.search(r"end-to-end|end to end|E2E", ln)
                and not re.search(r"tidak|bukan|luar scope|luar lingkup|out of scope", ln, re.I)]
    a.expect(not room_e2e, "no unqualified E2E claim for Public/Managed Room",
             f"{room_e2e[:2]}")

    return a


def selftest() -> int:
    """Break a copy of the documents on purpose and require the audit to say no.

    Section 178 says the script is tested this way, and the principle is the
    one the section states: a checker that has never failed proves nothing.
    Every cross-document check gets a mutation that breaks exactly it, applied
    to a fresh copy of the real documents, and the selftest fails unless the
    audit reports that specific check. The unmutated copy is audited first and
    must come back clean, so a mutation that fails for the wrong reason (or a
    harness bug that fails everything) cannot pass as coverage.
    """
    here = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix="brief-audit-selftest-") as td:
        tmp = Path(td)
        template = tmp / "template"
        (template / "shared" / "protocol" / "schema").mkdir(parents=True)
        (template / "docs").mkdir(parents=True)
        shutil.copy2(here / "migo.md", template / "migo.md")
        for j in ("meta", "opcodes", "enums", "errors"):
            shutil.copy2(here / "shared" / "protocol" / "schema" / f"{j}.json",
                         template / "shared" / "protocol" / "schema" / f"{j}.json")
        for d in ("02-protocol.md", "05-bandwidth-budget.md"):
            shutil.copy2(here / "docs" / d, template / "docs" / d)
        # The waveform bucket-count check reads the three client constants, so
        # the selftest template needs those files too — a mutation case below
        # breaks exactly one of them.
        for rel in ("clients/web/src/lib/migo/voice.ts",
                    "clients/android/app/src/main/kotlin/com/migo/app/media/Media.kt",
                    "clients/desktop/src/net/media.rs"):
            dest = template / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(here / rel, dest)

        control = run_audit(template, template / "migo.md", quiet=True)
        if control.problems:
            print("selftest FAIL  the unmutated documents already report problems:")
            for p in control.problems:
                print(f"    {p}")
            return 1
        print("  ok    the unmutated copy is clean")

        def edit(rel: str, old: str, new: str):
            def go(root: Path) -> None:
                target = root / rel
                t = target.read_text(encoding="utf-8")
                if t.count(old) != 1:
                    raise AssertionError(
                        f"anchor for {old!r} in {rel} found {t.count(old)} times, expected 1")
                target.write_text(t.replace(old, new), encoding="utf-8")
            return go

        def add_dangling_brief_ref(root: Path) -> None:
            # In a subdirectory on purpose: a top-level scan would never see it.
            d = root / "docs" / "adr"
            d.mkdir()
            (d / "selftest-drift.md").write_text("See brief §999.\n", encoding="utf-8")

        cases = [
            ("a limit loses its printed value",
             edit("migo.md", "MAX_MAP_ITEMS 1024", "MAX_MAP_ITEMS"),
             "limit values is printed beside its name"),
            ("section 72 reassigns a feature bit",
             edit("migo.md", "17 CALLS", "19 CALLS"),
             "section 72 feature-bit list matches meta.json"),
            ("the registry prints a wrong opcode code",
             edit("migo.md", "2 PING, dua arah", "3 PING, dua arah"),
             "section 145 opcode registry matches opcodes.json"),
            ("docs/02 hard limits drift from meta.json",
             edit("docs/02-protocol.md", "262 144", "262 145"),
             "hard-limit table matches meta.json"),
            ("docs/02 compression floor drifts",
             edit("docs/02-protocol.md", "COMPRESS_MIN_BYTES` (512)", "COMPRESS_MIN_BYTES` (513)"),
             "compression thresholds match meta.json"),
            ("docs/02 linger drifts",
             edit("docs/02-protocol.md", "≤ 15 ms** linger", "≤ 16 ms** linger"),
             "batch linger matches meta.json"),
            ("docs/02 backoff drifts from section 18",
             edit("docs/02-protocol.md", "backoff `1,2,4,8,16,30 s`", "backoff `1,2,4,8,16,60 s`"),
             "reconnect backoff matches section 18"),
            ("docs/02 heartbeat miss rule drifts",
             edit("docs/02-protocol.md", "Missing\n  2 intervals", "Missing\n  3 intervals"),
             "heartbeat miss rule matches section 18"),
            ("docs/05 budget drifts from section 56",
             edit("docs/05-bandwidth-budget.md",
                  "| Message receipt (delivered/read) | ≤ 24 B",
                  "| Message receipt (delivered/read) | ≤ 25 B"),
             "budget numbers match section 56"),
            ("section 180 resurrects a dead feature-bit name",
             edit("migo.md",
                  "adalah CALLS untuk 1-on-1 dan GROUP_CALL untuk group",
                  "adalah CALL_V1 untuk 1-on-1 dan GROUP_CALL_SFU_V1 untuk group"),
             "feature-bit claims are real bits in meta.json"),
            ("section 180 invents a call identifier",
             edit("migo.md",
                  "memicu re-keying melalui CALL_KEY_UPDATE",
                  "memicu re-keying melalui CALL_MEDIA_UPDATE"),
             "call identifiers appear in sections 165 and 166"),
            ("section 180 promises an unknown error",
             edit("migo.md", "Melewati batas dijawab QUOTA_EXCEEDED",
                  "Melewati batas dijawab QUOTA_EXCEEDED_V2"),
             "error symbols promised in sections 179 and 180 exist in errors.json"),
            ("section 180 participant limits drift",
             edit("migo.md", "32 peserta audio, dan paling banyak 8 stream video",
                  "30 peserta audio, dan paling banyak 8 stream video"),
             "participant limits match section 165"),
            ("section 180 drops a wire call state",
             edit("migo.md", "Connected\nReconnecting\nDegraded", "Connected\nDegraded"),
             "wire call states appear in the section 180 state list"),
            ("section 179 invents a media opcode",
             edit("migo.md", "melalui MEDIA_UPLOAD_STATUS", "melalui MEDIA_UPLOAD_PROGRESS"),
             "media opcodes appear in sections 167 and 168"),
            ("section 179 names an unknown enum",
             edit("migo.md", "mengikuti enum BandwidthMode pada section 75 supaya",
                  "mengikuti enum BandwidthModeV2 pada section 75 supaya"),
             "enums named in section 179 exist in enums.json"),
            ("section 179 claims a variant that does not exist",
             edit("migo.md", "ReceiptKind bernilai Delivered", "ReceiptKind bernilai Played"),
             "enum values claimed in section 179 are real variants"),
            ("section 179 drops a protocol playback speed",
             edit("migo.md", "Kecepatan 0.5x, 1x, 1.5x, dan 2x", "Kecepatan 0.5x, 1x, dan 2x"),
             "playback speeds appear in section 179"),
            ("section 179 invents a room permission",
             edit("migo.md", "dan VOICE_NOTE_PLAY", "dan VOICE_NOTE_TRANSCRIBE"),
             "voice note room permissions named in section 179 exist in section 48"),
            ("a client drifts its waveform bucket count from the brief",
             edit("clients/web/src/lib/migo/voice.ts", "WAVEFORM_BARS = 50", "WAVEFORM_BARS = 51"),
             "waveform bucket count is one number"),
            ("a docs subdirectory cites a nonexistent section",
             add_dangling_brief_ref,
             'references resolve'),
            # Anchored on the definition of the four words rather than on a
            # protocol section's own STATUS line, which is what it used to be.
            # That anchor was section 180's, and when section 180 was honestly
            # demoted from SPEC to SEBAGIAN -- work landed, and the line had to
            # say so -- this case failed on a missing anchor instead of on the
            # defect it exists for. The failure was visible rather than silent,
            # which is the design, but the case had retired itself for the one
            # reason that will keep happening: sections stop being SPEC. The
            # definition in section 0 is the line a vocabulary check should
            # mutate anyway, since it is the sentence that makes a fifth word
            # undefined in the first place, and it is in the frozen part of the
            # document.
            ("a section invents a fifth status word",
             edit("migo.md", "STATUS: SPEC\nBaru dispesifikasikan di dokumen ini.",
                  "STATUS: BUILD\nBaru dispesifikasikan di dokumen ini."),
             "four status words"),
        ]

        failures = []
        for i, (name, mutate, label) in enumerate(cases):
            root = tmp / f"case-{i:02d}"
            shutil.copytree(template, root)
            try:
                mutate(root)
            except AssertionError as exc:
                failures.append(f"{name}: {exc}")
                print(f"  FAIL  {name}")
                continue
            result = run_audit(root, root / "migo.md", quiet=True)
            hit = [p for p in result.problems if label in p]
            if not result.problems:
                failures.append(f"{name}: the audit stayed clean, the check never fired")
            elif not hit:
                failures.append(f"{name}: failed for other reasons: {result.problems[:2]}")
            print(f"  {'ok  ' if hit else 'FAIL'}  {name}")

        print(f"brief-audit selftest: {len(cases)} case(s), {len(failures)} failure(s)")
        for f in failures:
            print(f"  - {f}")
        return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parents[2]))
    ap.add_argument("--brief", default=None)
    ap.add_argument("--selftest", action="store_true",
                    help="break a copy of the documents on purpose and require "
                         "the audit to reject it")
    args = ap.parse_args()
    if args.selftest:
        return selftest()

    root = Path(args.root)
    brief_path = Path(args.brief) if args.brief else root / "migo.md"
    a = run_audit(root, brief_path)
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
