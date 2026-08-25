#!/usr/bin/env python3
"""Generate the wire conformance vectors in shared/protocol/vectors/wire/.

This is a second, independent implementation of MWP/1 framing and MSE, written
from docs/02-protocol.md sections 3 and 4 and from nothing else. It never imports
or executes the Rust crate, which is the entire point: if this file and
server/crates/migo-wire agree byte for byte, two people reading the same
specification arrived at the same encoding. If they disagree, one of them is
wrong and the vector run says which case.

The list of cases is hand-chosen — that judgement cannot be automated. The bytes
are computed, because hand arithmetic over ten-byte LEB128 groups is exactly the
kind of work a human gets subtly wrong and then enshrines as an expected value.

Usage:
    python3 tools/vectors/generate_wire_vectors.py [--check]

--check exits non-zero if the committed files differ from what this script
produces, which is how `make vectors` notices an edited-by-hand vector file.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

OUT_DIR = pathlib.Path(__file__).resolve().parents[2] / "shared" / "protocol" / "vectors" / "wire"

# --- limits, quoted from docs/02-protocol.md section 4 -----------------------

MAX_FRAME_BYTES = 262144
MAX_STRING_BYTES = 65536
MAX_BYTES_LEN = 131072
MAX_LIST_ITEMS = 4096
MAX_NESTING_DEPTH = 16
MAX_VARINT_BYTES = 10

PROTOCOL_VERSION = 1

FLAG_COMPRESSED = 0x01
FLAG_TRACED = 0x02
FLAG_BATCH = 0x04
FLAG_ERROR = 0x08
FLAG_ACK_REQUIRED = 0x10
FLAG_FRAGMENT = 0x20
FLAG_RESERVED_6 = 0x40
FLAG_FLAGS_EXT = 0x80


# --- primitives -------------------------------------------------------------


def leb128(value: int) -> bytes:
    """Unsigned LEB128: seven payload bits per byte, high bit as continuation."""
    if value < 0:
        raise ValueError("leb128 is unsigned")
    out = bytearray()
    while value >= 0x80:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value)
    return bytes(out)


def zigzag(value: int) -> int:
    """Maps a signed value onto an unsigned one: 0, -1, 1, -2 -> 0, 1, 2, 3.

    Written as the specification writes it. Python integers are unbounded, so the
    mask is what makes the arithmetic shift behave like a 64-bit one.
    """
    return ((value << 1) ^ (value >> 63)) & 0xFFFFFFFFFFFFFFFF


def encode_ops(ops: list[dict]) -> bytes:
    """Runs one MSE writer program and returns the bytes it produces."""
    out = bytearray()
    depth = 0
    for op in ops:
        kind = op["op"]
        if kind == "enter":
            depth += 1
            if depth > MAX_NESTING_DEPTH:
                raise ValueError("vector exceeds MAX_NESTING_DEPTH on the write side")
        elif kind == "leave":
            depth -= 1
        elif kind == "bool":
            out.append(1 if op["value"] else 0)
        elif kind in ("u32", "u64", "timestamp"):
            out += leb128(int(op["value"]))
        elif kind == "id":
            raw = bytes.fromhex(op["value"])
            assert len(raw) == 16, "an id is 16 bytes with no length prefix"
            out += raw
        elif kind == "string":
            raw = op["value"].encode("utf-8")
            assert len(raw) <= MAX_STRING_BYTES
            out += leb128(len(raw)) + raw
        elif kind == "bytes":
            raw = bytes.fromhex(op["value"])
            assert len(raw) <= MAX_BYTES_LEN
            out += leb128(len(raw)) + raw
        elif kind == "list_len":
            assert int(op["value"]) <= MAX_LIST_ITEMS
            out += leb128(int(op["value"]))
        elif kind == "optional":
            inner = encode_ops(op["ops"])
            out += leb128(op["id"]) + leb128(len(inner)) + inner
        else:
            raise ValueError(f"unknown op {kind!r}")
    return bytes(out)


def encode_frame(frame: dict) -> bytes:
    """MWP/1 header then payload, per docs/02-protocol.md section 3."""
    flags = frame["flags"] & ~(FLAG_TRACED | FLAG_FRAGMENT)
    if frame.get("trace"):
        flags |= FLAG_TRACED
    if frame.get("fragment"):
        flags |= FLAG_FRAGMENT
    if flags & (FLAG_RESERVED_6 | FLAG_FLAGS_EXT):
        raise ValueError("a valid frame cannot set a reserved flag bit")

    out = bytearray([frame["version"], flags])
    out += leb128(frame["opcode"])
    out += leb128(frame["correlation"])
    if trace := frame.get("trace"):
        trace_id = bytes.fromhex(trace["trace_id"])
        span_id = bytes.fromhex(trace["span_id"])
        assert len(trace_id) == 16 and len(span_id) == 8
        out += trace_id + span_id
    if fragment := frame.get("fragment"):
        assert fragment["total"] != 0 and fragment["index"] < fragment["total"]
        out += leb128(fragment["index"]) + leb128(fragment["total"])
    out += bytes.fromhex(frame["payload"])
    return bytes(out)


# --- case lists (hand-chosen) ----------------------------------------------


def varint_file() -> dict:
    values = [
        ("zero", 0),
        ("one", 1),
        ("largest_single_byte", 127),
        ("smallest_two_byte", 128),
        ("one_hundred_fifty", 150),
        ("byte_max", 255),
        ("three_hundred", 300),
        ("largest_two_byte", 16383),
        ("smallest_three_byte", 16384),
        ("u16_max", 65535),
        ("u32_max", 4294967295),
        ("smallest_ten_byte", 1 << 63),
        ("u64_max", (1 << 64) - 1),
    ]
    cases = [
        {"name": name, "value": str(value), "hex": leb128(value).hex()}
        for name, value in values
    ]

    signed = [
        ("zero", 0, 0),
        ("minus_one", -1, 1),
        ("one", 1, 2),
        ("minus_two", -2, 3),
        ("two", 2, 4),
        ("i32_min", -2147483648, 4294967295),
        ("i64_max", (1 << 63) - 1, (1 << 64) - 2),
        ("i64_min", -(1 << 63), (1 << 64) - 1),
    ]
    zz = []
    for name, value, expected in signed:
        got = zigzag(value) & 0xFFFFFFFFFFFFFFFF
        assert got == expected, f"zigzag({value}) computed {got}, table says {expected}"
        zz.append(
            {
                "name": name,
                "value": str(value),
                "encoded": str(expected),
                "hex": leb128(expected).hex(),
            }
        )

    invalid = [
        {
            "name": "empty_input",
            "hex": "",
            "error": "UnexpectedEnd",
            "why": "a varint needs at least one byte",
        },
        {
            "name": "continuation_bit_with_nothing_after_it",
            "hex": "80",
            "error": "UnexpectedEnd",
            "why": "the high bit promises another byte",
        },
        {
            "name": "two_byte_encoding_of_zero",
            "hex": "8000",
            "error": "NonMinimalVarint",
            "why": "zero has exactly one canonical encoding, 0x00",
        },
        {
            "name": "padded_encoding_of_one",
            "hex": "818000",
            "error": "NonMinimalVarint",
            "why": "a final group of zero means the value was padded",
        },
        {
            "name": "eleven_bytes",
            "hex": ("80" * MAX_VARINT_BYTES) + "00",
            "error": "VarintTooLong",
            "why": f"MAX_VARINT_BYTES is {MAX_VARINT_BYTES}; a longer run is a decoder spin",
        },
        {
            "name": "tenth_byte_carries_more_than_one_bit",
            "hex": ("ff" * 9) + "7f",
            "error": "VarintTooLong",
            "why": "the tenth byte may only supply bit 63 of a u64",
        },
    ]

    return {
        "$comment": "LEB128 varints: canonical encodings, and the non-canonical ones that must be rejected.",
        "provenance": "case list hand-chosen; bytes computed by tools/vectors/generate_wire_vectors.py from docs/02-protocol.md section 4",
        "cases": cases,
        "zigzag": zz,
        "invalid": invalid,
    }


TRACE_ID = "000102030405060708090a0b0c0d0e0f"
SPAN_ID = "1011121314151617"


def frames_file() -> dict:
    frames = [
        (
            "minimal",
            {
                "version": 1,
                "flags": 0,
                "opcode": 1,
                "correlation": 0,
                "trace": None,
                "fragment": None,
                "payload": "",
            },
        ),
        (
            "multi_byte_opcode_and_correlation",
            {
                "version": 1,
                "flags": 0,
                "opcode": 129,
                "correlation": 300,
                "trace": None,
                "fragment": None,
                "payload": "deadbeef",
            },
        ),
        (
            "error_flag",
            {
                "version": 1,
                "flags": FLAG_ERROR,
                "opcode": 2,
                "correlation": 7,
                "trace": None,
                "fragment": None,
                "payload": "",
            },
        ),
        (
            "ack_required_flag",
            {
                "version": 1,
                "flags": FLAG_ACK_REQUIRED,
                "opcode": 16,
                "correlation": 1,
                "trace": None,
                "fragment": None,
                "payload": "01",
            },
        ),
        (
            "compressed_flag_with_opaque_payload",
            {
                "version": 1,
                "flags": FLAG_COMPRESSED,
                "opcode": 20,
                "correlation": 2,
                "trace": None,
                "fragment": None,
                "payload": "cafebabe",
            },
        ),
        (
            "traced",
            {
                "version": 1,
                "flags": 0,
                "opcode": 1,
                "correlation": 0,
                "trace": {"trace_id": TRACE_ID, "span_id": SPAN_ID},
                "fragment": None,
                "payload": "",
            },
        ),
        (
            "fragmented",
            {
                "version": 1,
                "flags": 0,
                "opcode": 5,
                "correlation": 9,
                "trace": None,
                "fragment": {"index": 1, "total": 3},
                "payload": "aa",
            },
        ),
        (
            "traced_and_fragmented",
            {
                "version": 1,
                "flags": 0,
                "opcode": 5,
                "correlation": 0,
                "trace": {"trace_id": TRACE_ID, "span_id": SPAN_ID},
                "fragment": {"index": 0, "total": 2},
                "payload": "",
            },
        ),
        (
            "last_fragment",
            {
                "version": 1,
                "flags": 0,
                "opcode": 5,
                "correlation": 9,
                "trace": None,
                "fragment": {"index": 199, "total": 200},
                "payload": "bb",
            },
        ),
        (
            "opcode_at_u32_max",
            {
                "version": 1,
                "flags": 0,
                "opcode": 4294967295,
                "correlation": 0,
                "trace": None,
                "fragment": None,
                "payload": "",
            },
        ),
    ]

    cases = []
    for name, frame in frames:
        encoded = encode_frame(frame)
        expected = dict(frame)
        # `flags` in a case is what a decoder reports, so the derived bits are set.
        expected["flags"] = encoded[1]
        cases.append({"name": name, "frame": expected, "hex": encoded.hex()})

    length_prefixed = []
    for name in ("minimal", "multi_byte_opcode_and_correlation", "traced"):
        frame = next(f for n, f in frames if n == name)
        body = encode_frame(frame)
        length_prefixed.append(
            {
                "name": name,
                "frame_hex": body.hex(),
                "hex": len(body).to_bytes(4, "big").hex() + body.hex(),
            }
        )

    invalid = [
        {"name": "empty", "hex": "", "error": "UnexpectedEnd", "why": "a header is at least 2 bytes"},
        {"name": "version_byte_only", "hex": "01", "error": "UnexpectedEnd", "why": "the flags byte is missing"},
        {
            "name": "future_version",
            "hex": "02" + "00" + "01" + "00",
            "error": "UnsupportedVersion",
            "why": "a MWP/2 frame is not decoded as MWP/1 on a guess",
        },
        {
            "name": "version_zero",
            "hex": "00" + "00" + "01" + "00",
            "error": "UnsupportedVersion",
            "why": "there is no version 0",
        },
        {
            "name": "reserved_bit_6",
            "hex": "01" + f"{FLAG_RESERVED_6:02x}" + "01" + "00",
            "error": "ReservedFlags",
            "why": "ignoring the bit now makes it unusable forever",
        },
        {
            "name": "flags_ext_bit",
            "hex": "01" + f"{FLAG_FLAGS_EXT:02x}" + "01" + "00",
            "error": "ReservedFlags",
            "why": "the second flags byte is a MWP/2 feature",
        },
        {
            "name": "both_reserved_bits",
            "hex": "01" + f"{FLAG_RESERVED_6 | FLAG_FLAGS_EXT:02x}" + "01" + "00",
            "error": "ReservedFlags",
            "why": "reported as a mask, not one bit at a time",
        },
        {
            "name": "traced_but_trace_block_truncated",
            "hex": "01" + f"{FLAG_TRACED:02x}" + "01" + "00" + "0102",
            "error": "UnexpectedEnd",
            "why": "the flag promises 24 bytes and 2 are present",
        },
        {
            "name": "fragment_total_zero",
            "hex": "01" + f"{FLAG_FRAGMENT:02x}" + "05" + "09" + "00" + "00",
            "error": "InvalidFragment",
            "why": "nothing can be reassembled from zero fragments",
        },
        {
            "name": "fragment_index_equals_total",
            "hex": "01" + f"{FLAG_FRAGMENT:02x}" + "05" + "09" + "03" + "03",
            "error": "InvalidFragment",
            "why": "indices are zero-based, so index 3 of 3 does not exist",
        },
        {
            "name": "fragment_index_past_total",
            "hex": "01" + f"{FLAG_FRAGMENT:02x}" + "05" + "09" + "0a" + "02",
            "error": "InvalidFragment",
            "why": "a reassembly buffer must not be held open by a lie",
        },
        {
            "name": "opcode_past_u32",
            "hex": "01" + "00" + leb128(1 << 32).hex() + "00",
            "error": "FieldOverflow",
            "why": "varints decode as u64 and are then narrowed",
        },
        {
            "name": "non_minimal_opcode",
            "hex": "01" + "00" + "8000" + "00",
            "error": "NonMinimalVarint",
            "why": "canonicality applies to the header, not only to the payload",
        },
    ]

    return {
        "$comment": "MWP/1 frame headers: every flag combination that changes the layout, and the malformed headers a receiver must reject.",
        "provenance": "case list hand-chosen; bytes computed by tools/vectors/generate_wire_vectors.py from docs/02-protocol.md section 3",
        "note": "A COMPRESSED frame is carried opaquely here: raw DEFLATE output is not byte-stable across implementations, so only the header is pinned.",
        "cases": cases,
        "length_prefixed": length_prefixed,
        "invalid": invalid,
    }


ID_A = "0102030405060708090a0b0c0d0e0f10"
ID_ZERO = "00" * 16


def mse_file() -> dict:
    programs = [
        ("bool_false", [{"op": "bool", "value": False}]),
        ("bool_true", [{"op": "bool", "value": True}]),
        ("u32_zero", [{"op": "u32", "value": "0"}]),
        ("u32_multi_byte", [{"op": "u32", "value": "300"}]),
        ("u64_max", [{"op": "u64", "value": str((1 << 64) - 1)}]),
        ("timestamp_epoch", [{"op": "timestamp", "value": "0"}]),
        ("timestamp_one_day", [{"op": "timestamp", "value": "86400000"}]),
        ("id_is_sixteen_raw_bytes", [{"op": "id", "value": ID_A}]),
        ("id_all_zero", [{"op": "id", "value": ID_ZERO}]),
        ("string_empty", [{"op": "string", "value": ""}]),
        ("string_ascii", [{"op": "string", "value": "hello"}]),
        ("string_utf8_multibyte", [{"op": "string", "value": "halo — 世界 🌏"}]),
        ("bytes_empty", [{"op": "bytes", "value": ""}]),
        ("bytes_short", [{"op": "bytes", "value": "010203"}]),
        ("list_len_zero", [{"op": "list_len", "value": "0"}]),
        ("list_of_three_strings", [
            {"op": "list_len", "value": "3"},
            {"op": "string", "value": "a"},
            {"op": "string", "value": "bb"},
            {"op": "string", "value": "ccc"},
        ]),
        ("struct_with_no_optionals", [
            {"op": "enter"},
            {"op": "u64", "value": "42"},
            {"op": "u32", "value": "0"},
            {"op": "leave"},
        ]),
        ("struct_with_one_optional", [
            {"op": "enter"},
            {"op": "u64", "value": "42"},
            {"op": "u32", "value": "1"},
            {"op": "optional", "id": 1, "ops": [{"op": "string", "value": "hello"}]},
            {"op": "leave"},
        ]),
        ("struct_with_two_optionals", [
            {"op": "enter"},
            {"op": "id", "value": ID_A},
            {"op": "u32", "value": "2"},
            {"op": "optional", "id": 1, "ops": [{"op": "bool", "value": True}]},
            {"op": "optional", "id": 4, "ops": [{"op": "u32", "value": "7"}]},
            {"op": "leave"},
        ]),
        ("optional_id_needing_two_varint_bytes", [
            {"op": "enter"},
            {"op": "u32", "value": "1"},
            {"op": "optional", "id": 200, "ops": [{"op": "u32", "value": "1"}]},
            {"op": "leave"},
        ]),
        ("nested_struct_in_an_optional", [
            {"op": "enter"},
            {"op": "u32", "value": "1"},
            {"op": "optional", "id": 1, "ops": [
                {"op": "enter"},
                {"op": "string", "value": "inner"},
                {"op": "u32", "value": "0"},
                {"op": "leave"},
            ]},
            {"op": "leave"},
        ]),
        ("nested_optional_inside_an_optional", [
            {"op": "enter"},
            {"op": "u32", "value": "1"},
            {"op": "optional", "id": 1, "ops": [
                {"op": "enter"},
                {"op": "u32", "value": "1"},
                {"op": "optional", "id": 2, "ops": [{"op": "string", "value": "deep"}]},
                {"op": "leave"},
            ]},
            {"op": "leave"},
        ]),
        ("an_unknown_optional_field_is_skipped_by_length", [
            {"op": "enter"},
            {"op": "u64", "value": "7"},
            {"op": "u32", "value": "2"},
            {"op": "optional", "id": 1, "ops": [{"op": "string", "value": "hi"}]},
            {"op": "optional", "id": 99, "unknown": True, "ops": [
                {"op": "bytes", "value": "abcdef"},
            ]},
            {"op": "leave"},
        ]),
        ("only_unknown_optional_fields", [
            {"op": "enter"},
            {"op": "bool", "value": False},
            {"op": "u32", "value": "2"},
            {"op": "optional", "id": 40, "unknown": True, "ops": [{"op": "u64", "value": "1"}]},
            {"op": "optional", "id": 41, "unknown": True, "ops": [{"op": "string", "value": "x"}]},
            {"op": "leave"},
        ]),
        ("nesting_at_the_depth_limit", [
            *({"op": "enter"} for _ in range(MAX_NESTING_DEPTH)),
            {"op": "u32", "value": "1"},
            *({"op": "leave"} for _ in range(MAX_NESTING_DEPTH)),
        ]),
    ]

    cases = [
        {"name": name, "ops": ops, "hex": encode_ops(ops).hex()} for name, ops in programs
    ]

    invalid = [
        {
            "name": "bool_byte_two",
            "hex": "02",
            "read_ops": [{"op": "bool"}],
            "error": "InvalidBool",
            "why": "docs/02-protocol.md section 4: a bool is 0 or 1, anything else is a decode error",
        },
        {
            "name": "bool_byte_ff",
            "hex": "ff",
            "read_ops": [{"op": "bool"}],
            "error": "InvalidBool",
            "why": "truthiness is not canonical",
        },
        {
            "name": "string_length_past_end",
            "hex": "05" + "68656c",
            "read_ops": [{"op": "string"}],
            "error": "UnexpectedEnd",
            "why": "the prefix claims 5 bytes and 3 are present",
        },
        {
            "name": "string_over_the_limit",
            "hex": leb128(MAX_STRING_BYTES + 1).hex(),
            "read_ops": [{"op": "string"}],
            "error": "StringTooLong",
            "why": "checked against the limit before the buffer is present, let alone allocated",
        },
        {
            "name": "string_not_utf8",
            "hex": "02" + "fffe",
            "read_ops": [{"op": "string"}],
            "error": "InvalidUtf8",
            "why": "0xff 0xfe is not a UTF-8 sequence",
        },
        {
            "name": "string_truncated_utf8_sequence",
            "hex": "02" + "e4b8",
            "read_ops": [{"op": "string"}],
            "error": "InvalidUtf8",
            "why": "the first two bytes of a three-byte codepoint are not a string",
        },
        {
            "name": "bytes_over_the_limit",
            "hex": leb128(MAX_BYTES_LEN + 1).hex(),
            "read_ops": [{"op": "bytes"}],
            "error": "BytesTooLong",
            "why": "one ciphertext is the size this limit was chosen for",
        },
        {
            "name": "list_over_the_limit",
            "hex": leb128(MAX_LIST_ITEMS + 1).hex(),
            "read_ops": [{"op": "list_len"}],
            "error": "ListTooLong",
            "why": "a count is an allocation request from a stranger",
        },
        {
            "name": "list_count_larger_than_remaining_bytes",
            "hex": leb128(100).hex() + "0102",
            "read_ops": [{"op": "list_len"}],
            "error": "ListTooLong",
            "why": "every item costs at least one byte, so 100 items cannot fit in 2",
        },
        {
            "name": "id_truncated",
            "hex": "0102030405",
            "read_ops": [{"op": "id"}],
            "error": "UnexpectedEnd",
            "why": "an id has no length prefix, so a short buffer is the only signal",
        },
        {
            "name": "optional_length_past_end",
            "hex": "01" + "05" + "0102",
            "read_ops": [{"op": "optional"}],
            "error": "UnexpectedEnd",
            "why": "the field length must be inside the frame it was read from",
        },
        {
            "name": "trailing_bytes_after_the_value",
            "hex": "01" + "ff",
            "read_ops": [{"op": "u32"}],
            "error": "TrailingBytes",
            "why": "leftovers mean the two sides disagree about the schema",
        },
        {
            "name": "nesting_one_past_the_limit",
            "hex": "",
            "read_ops": [{"op": "enter"} for _ in range(MAX_NESTING_DEPTH + 1)],
            "error": "DepthExceeded",
            "why": f"MAX_NESTING_DEPTH is {MAX_NESTING_DEPTH}; deeper recursion is a stack attack",
        },
    ]

    return {
        "$comment": "Migo Struct Encoding: every primitive, optional-field layout, nesting, and unknown-field skipping.",
        "provenance": "case list hand-chosen; bytes computed by tools/vectors/generate_wire_vectors.py from docs/02-protocol.md section 4",
        "note": "A case is a writer program. The runner replays it through the encoder and compares bytes, then replays it through the decoder over those bytes and compares values. An op marked \"unknown\" is written like any other field but must be skipped, not read, on the way back. In `invalid`, an op with no \"value\" means read and discard, and `optional` with no \"id\" means read the field header only.",
        "cases": cases,
        "invalid": invalid,
    }


# --- driver -----------------------------------------------------------------

FILES = {
    "varint.json": varint_file,
    "frames.json": frames_file,
    "mse.json": mse_file,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if the committed files differ")
    args = parser.parse_args()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    stale = []
    for name, build in FILES.items():
        rendered = json.dumps(build(), indent=2, ensure_ascii=False) + "\n"
        path = OUT_DIR / name
        if args.check:
            current = path.read_text(encoding="utf-8") if path.exists() else ""
            if current != rendered:
                stale.append(name)
        else:
            path.write_text(rendered, encoding="utf-8")
            print(f"wrote {path.relative_to(OUT_DIR.parents[3])}")

    if stale:
        print("stale wire vectors: " + ", ".join(stale), file=sys.stderr)
        print("run: python3 tools/vectors/generate_wire_vectors.py", file=sys.stderr)
        return 1
    if args.check:
        print(f"up to date: {len(FILES)} wire vector files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
