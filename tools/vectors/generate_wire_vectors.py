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
MAX_BATCH_ITEMS = 256
COMPRESS_MIN_BYTES = 512
COMPRESS_MIN_GAIN_PERCENT = 10

PROTOCOL_VERSION = 1

FLAG_COMPRESSED = 0x01
FLAG_TRACED = 0x02
FLAG_BATCH = 0x04
FLAG_ERROR = 0x08
FLAG_ACK_REQUIRED = 0x10
FLAG_FRAGMENT = 0x20
FLAG_METADATA = 0x40
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
    flags = frame["flags"] & ~(FLAG_TRACED | FLAG_FRAGMENT | FLAG_METADATA)
    if frame.get("trace"):
        flags |= FLAG_TRACED
    if frame.get("fragment"):
        flags |= FLAG_FRAGMENT
    if frame.get("metadata"):
        flags |= FLAG_METADATA
    if flags & FLAG_FLAGS_EXT:
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
    if metadata := frame.get("metadata"):
        out += leb128(metadata["frame_seq"]) + leb128(metadata["sent_at_delta"])
        # The block is always three varints. A trailing optional varint cannot
        # be decoded unambiguously — nothing on the wire would distinguish "the
        # block ended" from "the block continues" — so payload_len is always
        # written and a value of zero means "not stated". The semantic presence
        # is carried by the value, not by the byte's existence.
        out += leb128(metadata.get("payload_len", 0))
    out += bytes.fromhex(frame["payload"])
    return bytes(out)


def frame_spec(
    flags: int = 0,
    opcode: int = 0,
    correlation: int = 0,
    trace: dict | None = None,
    fragment: dict | None = None,
    metadata: dict | None = None,
    payload: bytes = b"",
) -> tuple[dict, bytes]:
    """Builds one frame for a batch element, returning its case spec and its bytes.

    The spec's `flags` is what a decoder reports — the derived bits set — so the
    runner can rebuild the frame from the spec and compare headers after a round
    trip through the envelope, exactly the way `frames_file` cases work.
    """
    frame = {
        "version": PROTOCOL_VERSION,
        "flags": flags,
        "opcode": opcode,
        "correlation": correlation,
        "trace": trace,
        "fragment": fragment,
        "metadata": metadata,
        "payload": payload.hex(),
    }
    encoded = encode_frame(frame)
    spec = dict(frame)
    spec["flags"] = encoded[1]
    return spec, encoded


def batch_payload(elements: list[bytes]) -> bytes:
    """The payload of a BATCH envelope: varint count, then length-prefixed frames."""
    out = bytearray(leb128(len(elements)))
    for encoded in elements:
        out += leb128(len(encoded))
        out += encoded
    return bytes(out)


def envelope_spec(payload: bytes, flags: int) -> tuple[dict, bytes]:
    """Builds the BATCH envelope frame around `payload`, as a case spec and bytes."""
    frame = {
        "version": PROTOCOL_VERSION,
        "flags": flags,
        "opcode": 0,
        "correlation": 0,
        "trace": None,
        "fragment": None,
        "metadata": None,
        "payload": payload.hex(),
    }
    encoded = encode_frame(frame)
    spec = dict(frame)
    spec["flags"] = encoded[1]
    return spec, encoded


# --- raw DEFLATE (RFC 1951) -------------------------------------------------
#
# The COMPRESSED vectors need streams whose bytes cannot drift with a compressor
# version or its tuning, so every stream is authored here at the bit level from
# the RFC's own tables and then self-checked against zlib's *inflater* before
# emission. zlib's compressor is never asked for a single byte: decoding is
# specified exactly, compressing is not, and only the specified half belongs in
# a committed vector file.


class DeflateBits:
    """A bit-level DEFLATE writer: data elements LSB-first, Huffman codes MSB-first."""

    def __init__(self) -> None:
        self._out = bytearray()
        self._accumulator = 0
        self._count = 0

    def data(self, value: int, bits: int) -> None:
        """A non-Huffman element, packed starting from its least-significant bit."""
        assert 0 <= value < (1 << bits), f"{value} does not fit {bits} bits"
        self._accumulator |= value << self._count
        self._count += bits
        while self._count >= 8:
            self._out.append(self._accumulator & 0xFF)
            self._accumulator >>= 8
            self._count -= 8

    def code(self, value: int, bits: int) -> None:
        """A Huffman code, packed starting from its most-significant bit."""
        for shift in range(bits - 1, -1, -1):
            self.data((value >> shift) & 1, 1)

    def finish(self) -> bytes:
        if self._count:
            self._out.append(self._accumulator & 0xFF)
        return bytes(self._out)


def fixed_literal(bits: DeflateBits, symbol: int) -> None:
    """The fixed-Huffman literal/length code for `symbol`, RFC 1951 section 3.2.6."""
    if symbol <= 143:
        bits.code(0x30 + symbol, 8)
    elif symbol <= 255:
        bits.code(0x190 + symbol - 144, 9)
    elif symbol <= 279:
        bits.code(symbol - 256, 7)
    else:
        assert symbol <= 287
        bits.code(0xC0 + symbol - 280, 8)


# (symbol, base length, extra bits) — RFC 1951 section 3.2.5.
LENGTH_CODES = [
    (257, 3, 0), (258, 4, 0), (259, 5, 0), (260, 6, 0), (261, 7, 0), (262, 8, 0),
    (263, 9, 0), (264, 10, 0), (265, 11, 1), (266, 13, 1), (267, 15, 1), (268, 17, 1),
    (269, 19, 2), (270, 23, 2), (271, 27, 2), (272, 31, 2), (273, 35, 3), (274, 43, 3),
    (275, 51, 3), (276, 59, 3), (277, 67, 4), (278, 83, 4), (279, 99, 4), (280, 115, 4),
    (281, 131, 5), (282, 163, 5), (283, 195, 5), (284, 227, 5), (285, 258, 0),
]

# (symbol, base distance, extra bits) — RFC 1951 section 3.2.5, read as the
# fixed 5-bit distance code plus its extra bits.
DISTANCE_CODES = [
    (0, 1, 0), (1, 2, 0), (2, 3, 0), (3, 4, 0), (4, 5, 1), (5, 7, 1), (6, 9, 2),
    (7, 13, 2), (8, 17, 3), (9, 25, 3), (10, 33, 4), (11, 49, 4), (12, 65, 5),
    (13, 97, 5), (14, 129, 6), (15, 193, 6), (16, 257, 7), (17, 385, 7), (18, 513, 8),
    (19, 769, 8), (20, 1025, 9), (21, 1537, 9), (22, 2049, 10), (23, 3073, 10),
    (24, 4097, 11), (25, 6145, 11), (26, 8193, 12), (27, 12289, 12), (28, 16385, 13),
    (29, 24577, 13),
]


def _covering(table: list[tuple[int, int, int]], value: int) -> tuple[int, int, int]:
    """The first table entry whose range covers `value`."""
    for symbol, base, extra in table:
        if base <= value < base + (1 << extra):
            return symbol, base, extra
    raise ValueError(f"no code covers {value}")


def deflate_stored(blocks: list[tuple[bytes, bool]]) -> bytes:
    """Stored blocks: each is (data, final). No compression, but a real DEFLATE stream."""
    out = bytearray()
    for data, final in blocks:
        assert len(data) <= 0xFFFF, "a stored block holds at most 65535 bytes"
        out.append(1 if final else 0)  # BFINAL, then BTYPE=00 and zero padding
        out += len(data).to_bytes(2, "little")
        out += (~len(data) & 0xFFFF).to_bytes(2, "little")
        out += data
    return bytes(out)


def deflate_fixed(program: list[tuple, ...], final: bool = True) -> bytes:
    """A fixed-Huffman block from a program of ("lit", byte) and ("match", len, dist)."""
    bits = DeflateBits()
    bits.data(1 if final else 0, 1)  # BFINAL
    bits.data(1, 2)  # BTYPE=01, fixed Huffman
    for item in program:
        if item[0] == "lit":
            fixed_literal(bits, item[1])
        else:
            _, length, distance = item
            symbol, base, extra = _covering(LENGTH_CODES, length)
            fixed_literal(bits, symbol)
            if extra:
                bits.data(length - base, extra)
            dsymbol, dbase, dextra = _covering(DISTANCE_CODES, distance)
            bits.code(dsymbol, 5)
            if dextra:
                bits.data(distance - dbase, dextra)
    fixed_literal(bits, 256)  # end of block
    return bits.finish()


def deflate_runs(data: bytes) -> bytes:
    """A fixed-Huffman block for `data`: literals, with runs of one byte as matches.

    A whole general-purpose LZ77 is more machinery than a vector file needs. This
    spelling is enough to exercise both the literal and the match paths of a
    decoder, including multi-match inputs, while staying obvious to read.
    """
    program: list[tuple, ...] = []
    i = 0
    while i < len(data):
        run = 1
        while i + run < len(data) and data[i + run] == data[i] and run < 259:
            run += 1
        if run >= 4:
            program.append(("lit", data[i]))
            program.append(("match", run - 1, 1))
        else:
            program.extend(("lit", data[i + j]) for j in range(run))
        i += run
    return deflate_fixed(program)


def inflate_check(name: str, stream: bytes, plain: bytes) -> None:
    """Self-check: what was authored must inflate, and to exactly `plain`.

    zlib's inflater is the RFC's reference implementation in practice, and this
    check is what turns 'hand-written bits' into 'hand-written bits that a second
    implementation agrees are DEFLATE'. zlib's compressor stays out of it, so the
    committed bytes never depend on a zlib version.
    """
    import zlib

    restored = zlib.decompress(stream, wbits=-15)
    assert restored == plain, f"{name}: authored stream inflates to the wrong bytes"


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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
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
                "metadata": None,
                "payload": "",
            },
        ),
        (
            "metadata_block",
            {
                "version": 1,
                "flags": 0,
                "opcode": 40,
                "correlation": 12,
                "trace": None,
                "fragment": None,
                "metadata": {"frame_seq": 7, "sent_at_delta": 300},
                "payload": "0011",
            },
        ),
        (
            "metadata_block_with_payload_len",
            {
                "version": 1,
                "flags": 0,
                "opcode": 40,
                "correlation": 12,
                "trace": None,
                "fragment": None,
                "metadata": {"frame_seq": 7, "sent_at_delta": 300, "payload_len": 2},
                "payload": "0011",
            },
        ),
        (
            "metadata_block_with_multi_byte_fields",
            {
                "version": 1,
                "flags": 0,
                "opcode": 1,
                "correlation": 0,
                "trace": None,
                "fragment": None,
                "metadata": {"frame_seq": 4294967295, "sent_at_delta": 86400000},
                "payload": "",
            },
        ),
        (
            "metadata_with_trace_and_fragment",
            {
                "version": 1,
                "flags": 0,
                "opcode": 5,
                "correlation": 3,
                "trace": {"trace_id": TRACE_ID, "span_id": SPAN_ID},
                "fragment": {"index": 1, "total": 2},
                "metadata": {"frame_seq": 9001, "sent_at_delta": 65535, "payload_len": 128},
                "payload": "cc",
            },
        ),
        (
            "metadata_with_ack_required",
            {
                "version": 1,
                "flags": FLAG_ACK_REQUIRED,
                "opcode": 16,
                "correlation": 4,
                "trace": None,
                "fragment": None,
                "metadata": {"frame_seq": 1, "sent_at_delta": 15},
                "payload": "01",
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
    for name in ("minimal", "multi_byte_opcode_and_correlation", "traced", "metadata_block"):
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
            "name": "flags_ext_bit",
            "hex": "01" + f"{FLAG_FLAGS_EXT:02x}" + "01" + "00",
            "error": "ReservedFlags",
            "why": "the second flags byte is a MWP/2 feature",
        },
        {
            "name": "metadata_frame_seq_truncated",
            "hex": "01" + f"{FLAG_METADATA:02x}" + "01" + "00" + "80",
            "error": "UnexpectedEnd",
            "why": "the metadata block promises a varint and cuts it mid-byte",
        },
        {
            "name": "metadata_sent_at_delta_missing",
            "hex": "01" + f"{FLAG_METADATA:02x}" + "01" + "00",
            "error": "UnexpectedEnd",
            "why": "the flag promises a metadata block and zero bytes of it are present",
        },
        {
            "name": "metadata_payload_len_missing",
            "hex": "01" + f"{FLAG_METADATA:02x}" + "01" + "00" + "07" + leb128(300).hex(),
            "error": "UnexpectedEnd",
            "why": "the block is always three varints, so a block that stops at two is truncated",
        },
        {
            "name": "metadata_frame_seq_past_u32",
            "hex": "01" + f"{FLAG_METADATA:02x}" + "01" + "00" + leb128(1 << 32).hex(),
            "error": "FieldOverflow",
            "why": "frame_seq is a u32; the varint decodes as u64 and is then narrowed",
        },
        {
            "name": "metadata_payload_len_past_u32",
            "hex": "01"
            + f"{FLAG_METADATA:02x}"
            + "01"
            + "00"
            + leb128(1).hex()
            + leb128(1).hex()
            + leb128(1 << 32).hex(),
            "error": "FieldOverflow",
            "why": "a payload_len that does not fit u32 cannot describe a frame under the limit",
        },
        {
            "name": "metadata_frame_seq_non_minimal",
            "hex": "01" + f"{FLAG_METADATA:02x}" + "01" + "00" + "8000" + "01",
            "error": "NonMinimalVarint",
            "why": "canonicality applies to the metadata block too",
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
        {
            "name": "u32_past_the_field_width",
            "hex": leb128(1 << 32).hex(),
            "read_ops": [{"op": "u32"}],
            "error": "LengthOverflow",
            "why": "varints decode as u64 and are then narrowed; a u32 field that does not fit is refused, not wrapped",
        },
    ]

    return {
        "$comment": "Migo Struct Encoding: every primitive, optional-field layout, nesting, and unknown-field skipping.",
        "provenance": "case list hand-chosen; bytes computed by tools/vectors/generate_wire_vectors.py from docs/02-protocol.md section 4",
        "note": "A case is a writer program. The runner replays it through the encoder and compares bytes, then replays it through the decoder over those bytes and compares values. An op marked \"unknown\" is written like any other field but must be skipped, not read, on the way back. In `invalid`, an op with no \"value\" means read and discard, and `optional` with no \"id\" means read the field header only.",
        "cases": cases,
        "invalid": invalid,
    }


# --- BATCH and COMPRESSED ---------------------------------------------------


def batch_file() -> dict:
    """The BATCH envelope: packing, unpacking, and the hostile payloads it refuses."""
    cases = []

    def packed_case(name: str, specs: list[dict], encoded: list[bytes]) -> None:
        spec, envelope = envelope_spec(batch_payload(encoded), FLAG_BATCH)
        cases.append({"name": name, "elements": specs, "frame": spec, "hex": envelope.hex()})

    empty = batch_payload([])
    packed_case("empty_batch", [], [])
    minimal_a, minimal_a_bytes = frame_spec(opcode=0x30, payload=b"a")
    minimal_b, minimal_b_bytes = frame_spec(opcode=0x31, correlation=1, payload=b"bc")
    # A real inner envelope, since the packer refuses to produce one itself: the
    # element below is a well-formed frame whose own flags carry BATCH.
    inner_payload = batch_payload([minimal_a_bytes, minimal_b_bytes])
    _, inner_envelope = envelope_spec(inner_payload, FLAG_BATCH)
    packed_case(
        "two_small_frames",
        [minimal_a, minimal_b],
        [minimal_a_bytes, minimal_b_bytes],
    )
    error_spec, error_bytes = frame_spec(flags=FLAG_ERROR, opcode=2, correlation=7)
    ack_spec, ack_bytes = frame_spec(
        flags=FLAG_ACK_REQUIRED, opcode=16, correlation=1, payload=b"\x01"
    )
    packed_case(
        "elements_keep_their_own_flags",
        [error_spec, ack_spec],
        [error_bytes, ack_bytes],
    )
    traced_spec, traced_bytes = frame_spec(
        opcode=1, trace={"trace_id": TRACE_ID, "span_id": SPAN_ID}
    )
    fragmented_spec, fragmented_bytes = frame_spec(
        opcode=5, correlation=9, fragment={"index": 1, "total": 3}, payload=b"\xaa"
    )
    packed_case(
        "traced_and_fragmented_elements",
        [traced_spec, fragmented_spec],
        [traced_bytes, fragmented_bytes],
    )
    metadata_spec, metadata_bytes = frame_spec(
        opcode=40,
        correlation=12,
        metadata={"frame_seq": 7, "sent_at_delta": 300},
        payload=b"\x00\x11",
    )
    packed_case(
        "a_metadata_element_rides_along",
        [metadata_spec, minimal_a],
        [metadata_bytes, minimal_a_bytes],
    )
    # The item cap is a boundary, so both sides of it are pinned: 256 here, 257 in
    # `invalid`. Distinct correlations mean a decoder that reorders or drops an
    # element cannot pass by accident.
    limit_specs = []
    limit_bytes = []
    for i in range(MAX_BATCH_ITEMS):
        spec, encoded = frame_spec(opcode=0x40, correlation=i)
        limit_specs.append(spec)
        limit_bytes.append(encoded)
    packed_case("count_at_the_item_limit", limit_specs, limit_bytes)

    # A lone frame is sent bare: wrapping it would add bytes and buy nothing.
    lone_spec, lone_bytes = frame_spec(opcode=0x30, correlation=0, payload=b"solo")
    cases.append(
        {
            "name": "a_lone_frame_is_sent_bare",
            "elements": [lone_spec],
            "frame": lone_spec,
            "hex": lone_bytes.hex(),
        }
    )

    # A compressed envelope. The elements share vocabulary, so the payload is
    # deflated as a whole; the runner decodes this case rather than re-encoding
    # it, because two conforming DEFLATE encoders may emit different bytes.
    run_elements = [
        frame_spec(opcode=0x30, payload=b"a" * 40),
        frame_spec(opcode=0x31, correlation=1, payload=b"b" * 40),
        frame_spec(flags=FLAG_ACK_REQUIRED, opcode=16, correlation=2, payload=b"\x01" * 40),
    ]
    run_specs = [spec for spec, _ in run_elements]
    run_bytes = [encoded for _, encoded in run_elements]
    run_payload = batch_payload(run_bytes)
    run_stream = deflate_runs(run_payload)
    inflate_check("compressed_batch_of_run_payloads", run_stream, run_payload)
    run_spec, run_envelope = envelope_spec(run_stream, FLAG_BATCH | FLAG_COMPRESSED)
    compressed_cases = [
        {
            "name": "compressed_batch_of_run_payloads",
            "elements": run_specs,
            "frame": run_spec,
            "hex": run_envelope.hex(),
        }
    ]

    invalid = [
        {
            "name": "count_over_the_item_limit",
            "hex": envelope_spec(leb128(MAX_BATCH_ITEMS + 1) + b"\x00" * 6, FLAG_BATCH)[1].hex(),
            "error": "BatchTooLarge",
            "why": f"MAX_BATCH_ITEMS is {MAX_BATCH_ITEMS}; the count is a batch, not a suggestion",
        },
        {
            "name": "a_lying_count_cannot_force_an_allocation",
            "hex": envelope_spec(leb128(200) + b"\x00" * 6, FLAG_BATCH)[1].hex(),
            "error": "BatchTooLarge",
            "why": "every element costs at least five bytes, so 200 items cannot fit in six",
        },
        {
            "name": "nested_batch_is_refused",
            "hex": envelope_spec(
                leb128(1) + leb128(len(inner_envelope)) + inner_envelope, FLAG_BATCH
            )[1].hex(),
            "error": "NestedBatch",
            "why": "a batch inside a batch is an exponential expansion in a small frame",
        },
        # The three cases below pad their payloads past the plausibility check
        # (`a_lying_count_cannot_force_an_allocation`) on purpose, so that the
        # error each one names is the one a conforming decoder reports and not
        # BatchTooLarge: an element length over the frame budget, a declared
        # element of zero bytes, and a length varint cut off mid-byte.
        {
            "name": "element_length_over_the_frame_budget",
            "hex": envelope_spec(
                leb128(1) + leb128(MAX_FRAME_BYTES + 1) + b"\x00\x00", FLAG_BATCH
            )[1].hex(),
            "error": "FrameTooLarge",
            "why": f"an element is a frame, and a frame over MAX_FRAME_BYTES ({MAX_FRAME_BYTES}) is refused before buffering; the two padding bytes exist so the count pre-check lets the length varint be read at all",
        },
        {
            "name": "element_of_zero_bytes",
            "hex": envelope_spec(leb128(1) + leb128(0) + b"\x00" * 4, FLAG_BATCH)[1].hex(),
            "error": "UnexpectedEnd",
            "why": "a frame is at least two bytes, so an element of length zero is truncated; the padding exists so the count pre-check lets the zero-length element be reached at all",
        },
        {
            "name": "element_length_varint_truncated",
            "hex": envelope_spec(leb128(1) + b"\x80" * 5, FLAG_BATCH)[1].hex(),
            "error": "UnexpectedEnd",
            "why": "the continuation bit keeps promising a length byte that never arrives; the payload is long enough that the count pre-check cannot answer first",
        },
        {
            "name": "truncated_element",
            "hex": envelope_spec(
                leb128(1) + leb128(len(lone_bytes)) + lone_bytes[:-4], FLAG_BATCH
            )[1].hex(),
            "error": "UnexpectedEnd",
            "why": f"the element's length varint says {len(lone_bytes)} bytes and {len(lone_bytes) - 4} are present",
        },
        {
            "name": "trailing_bytes_after_the_last_element",
            "hex": envelope_spec(
                batch_payload([minimal_a_bytes, minimal_b_bytes]) + b"junk", FLAG_BATCH
            )[1].hex(),
            "error": "TrailingBytes",
            "why": "the count is the whole truth: bytes after the last element mean the sender disagrees",
        },
        {
            "name": "non_minimal_count",
            "hex": envelope_spec(b"\x80\x00", FLAG_BATCH)[1].hex(),
            "error": "NonMinimalVarint",
            "why": "canonicality applies to the count, not only to MSE fields",
        },
        {
            "name": "count_varint_truncated",
            "hex": envelope_spec(b"\x80", FLAG_BATCH)[1].hex(),
            "error": "UnexpectedEnd",
            "why": "the envelope promises a count and cuts it mid-byte",
        },
        {
            "name": "empty_payload",
            "hex": envelope_spec(b"", FLAG_BATCH)[1].hex(),
            "error": "UnexpectedEnd",
            "why": "a batch payload is at least the count varint",
        },
    ]

    return {
        "$comment": "The BATCH envelope: whole frames packed into one transport message, the compressed envelope, and the hostile payloads a receiver must refuse.",
        "provenance": "case list hand-chosen; envelope bytes computed by tools/vectors/generate_wire_vectors.py from migo.md sections 140, 154 and 155; the DEFLATE stream in `compressed_cases` is written bit by bit from RFC 1951 and self-checked against zlib's inflater before emission",
        "note": "`cases` are both directions: the elements must pack to `hex`, and `hex` must unpack to the elements with the envelope header in `frame`. `compressed_cases` are decode-only — a COMPRESSED envelope's payload is raw DEFLATE, whose exact bytes are not pinned across implementations (see compress.json) — so the runner decodes `hex` and unpacks it rather than re-encoding. A frame without the BATCH flag unpacks to itself, which is why `a_lone_frame_is_sent_bare` has no envelope.",
        "cases": cases,
        "compressed_cases": compressed_cases,
        "invalid": invalid,
    }


def _xorshift_bytes(count: int, seed: int) -> bytes:
    """Pseudo-random bytes from xorshift64: deterministic, and incompressible in practice."""
    state = seed
    out = bytearray()
    for _ in range(count):
        state ^= state << 13
        state ^= state >> 7
        state ^= state << 17
        out.append((state >> 24) & 0xFF)
    return bytes(out)


def compress_file() -> dict:
    """Raw DEFLATE: the streams a COMPRESSED frame carries, the policy, and the refusals."""

    # --- streams, decode-direction pinned -----------------------------------

    empty_stream = deflate_fixed([])
    inflate_check("empty_stream", empty_stream, b"")
    stored_plain = bytes([0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF, 0x41, 0x42, 0x43, 0x44])
    stored_stream = deflate_stored([(stored_plain, True)])
    inflate_check("stored_block", stored_stream, stored_plain)
    literal_plain = b"halo dunia"
    literal_stream = deflate_runs(literal_plain)
    inflate_check("fixed_block_of_literals", literal_stream, literal_plain)
    match_stream = deflate_fixed(
        [("lit", 0x61), ("lit", 0x62), ("match", 46, 2)]
    )
    match_plain = b"ab" * 24
    inflate_check("fixed_block_with_a_match", match_stream, match_plain)
    two_block_plain = b"s" * 50 + b"end"
    two_block_stream = deflate_stored([(b"s" * 50, False)]) + deflate_runs(b"end")
    inflate_check("stored_then_fixed_blocks", two_block_stream, two_block_plain)
    run_plain = b"\x00" * 1000
    run_stream = deflate_runs(run_plain)
    inflate_check("run_of_a_thousand_zeros", run_stream, run_plain)
    # One 8 KiB decoder chunk plus 529 bytes: the shape the gateway suite found
    # in CI when a decoder promised its whole output fit a single read. A
    # decoder that inflates through a fixed-size buffer must loop, and this size
    # is chosen so the loop cannot be skipped by luck.
    long_plain = b"\x61" * (8 * 1024 + 529)
    long_stream = deflate_runs(long_plain)
    inflate_check("run_past_the_decoder_chunk", long_stream, long_plain)

    cases = [
        {"name": "empty_stream", "plain_hex": "", "compressed_hex": empty_stream.hex()},
        {"name": "stored_block", "plain_hex": stored_plain.hex(), "compressed_hex": stored_stream.hex()},
        {"name": "fixed_block_of_literals", "plain_hex": literal_plain.hex(), "compressed_hex": literal_stream.hex()},
        {"name": "fixed_block_with_a_match", "plain_hex": match_plain.hex(), "compressed_hex": match_stream.hex()},
        {"name": "stored_then_fixed_blocks", "plain_hex": two_block_plain.hex(), "compressed_hex": two_block_stream.hex()},
        {"name": "run_of_a_thousand_zeros", "plain_hex": run_plain.hex(), "compressed_hex": run_stream.hex()},
        {"name": "run_past_the_decoder_chunk", "plain_hex": long_plain.hex(), "compressed_hex": long_stream.hex()},
    ]

    # --- whole frames with the COMPRESSED flag -------------------------------

    frame_plain = b"payload-" * 72
    frame_stream = deflate_runs(frame_plain)
    inflate_check("compressed_frame", frame_stream, frame_plain)
    spec, encoded = frame_spec(
        flags=FLAG_COMPRESSED, opcode=0x21, correlation=5, payload=frame_stream
    )
    frames = [
        {
            "name": "compressed_frame_inflates_to_its_payload",
            "frame": spec,
            "hex": encoded.hex(),
            "plain_hex": frame_plain.hex(),
        }
    ]

    # --- the policy: when a sender may compress ------------------------------

    floor_incompressible = _xorshift_bytes(COMPRESS_MIN_BYTES, 0x2545F4914F6CDD1D)
    above_floor_incompressible = _xorshift_bytes(COMPRESS_MIN_BYTES * 4, 0x9E3779B97F4A7C15)
    policy = [
        {
            "name": "below_the_floor_is_never_compressed",
            "plain_hex": (b"a" * (COMPRESS_MIN_BYTES - 1)).hex(),
            "compresses": False,
            "why": "the header costs more than the saving on a small payload",
        },
        {
            "name": "at_the_floor_and_compressible",
            "plain_hex": (b"a" * COMPRESS_MIN_BYTES).hex(),
            "compresses": True,
            "why": "highly redundant input at exactly COMPRESS_MIN_BYTES",
        },
        {
            "name": "at_the_floor_and_incompressible",
            "plain_hex": floor_incompressible.hex(),
            "compresses": False,
            "why": "random bytes have no redundancy; sending them larger helps nobody",
        },
        {
            "name": "well_above_the_floor_and_incompressible",
            "plain_hex": above_floor_incompressible.hex(),
            "compresses": False,
            "why": "size alone does not earn compression; the gain must",
        },
    ]

    # --- refusals -------------------------------------------------------------

    bomb_plain = b"B" * (MAX_FRAME_BYTES + 1)
    bomb_program: list[tuple, ...] = [("lit", 0x42)]
    remaining = len(bomb_plain) - 1
    while remaining >= 3:
        take = min(remaining, 258)
        bomb_program.append(("match", take, 1))
        remaining -= take
    for _ in range(remaining):
        bomb_program.append(("lit", 0x42))
    bomb_stream = deflate_fixed(bomb_program)

    invalid = [
        {
            "name": "not_deflate_at_all",
            "hex": "ffffffff",
            "error": "DecompressFailed",
            "why": "no DEFLATE block starts with those bits",
        },
        {
            "name": "truncated_fixed_block",
            "hex": match_stream[:-1].hex(),
            "error": "DecompressFailed",
            "why": "the final block is cut mid-code; a truncated stream is not a short one",
        },
        {
            "name": "stored_block_shorter_than_it_claims",
            "hex": deflate_stored([(stored_plain, True)])[: 5 + 6].hex(),
            "error": "DecompressFailed",
            "why": "LEN says ten bytes of data and the stream ends after six",
        },
        {
            "name": "decompression_bomb",
            "hex": bomb_stream.hex(),
            "error": "DecompressedTooLarge",
            "why": f"inflates past the {MAX_FRAME_BYTES} byte frame limit; bounded inflation is not optional",
            "expands_to": str(len(bomb_plain)),
        },
    ]

    return {
        "$comment": "Raw DEFLATE payloads: the streams a COMPRESSED frame carries, the policy that decides whether to compress, and the malformed streams a receiver must refuse.",
        "provenance": "case list hand-chosen; every DEFLATE stream is written bit by bit from RFC 1951 by tools/vectors/generate_wire_vectors.py and self-checked against zlib's inflater before emission — no compressor output is pinned anywhere, so the files cannot drift with a zlib version",
        "note": "Two conforming DEFLATE encoders may emit different bytes for the same input, so `cases` pin the decode direction only: `compressed_hex` must inflate to `plain_hex`. The encode direction is asserted by each runner as its own deflate-then-inflate round trip, and `policy` pins the decision (floor and gain), never the bytes. `frames` are complete MWP/1 frames whose COMPRESSED payload must inflate to `plain_hex`.",
        "cases": cases,
        "frames": frames,
        "policy": policy,
        "invalid": invalid,
    }


# --- driver -----------------------------------------------------------------

FILES = {
    "varint.json": varint_file,
    "frames.json": frames_file,
    "mse.json": mse_file,
    "batch.json": batch_file,
    "compress.json": compress_file,
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
