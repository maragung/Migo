//! Runs the cross-language conformance vectors in `shared/protocol/vectors/wire`.
//!
//! These are the highest-value tests in the crate, and the reason is structural:
//! every other test here compares this implementation to itself. A round-trip
//! test passes just as happily when the encoder and the decoder share a mistake,
//! and a property test explores the space that the code already agrees on. Only a
//! vector file pins the bytes against something outside the crate — in this case
//! a second implementation written from `docs/02-protocol.md` in Python, which
//! never sees this code.
//!
//! Both directions are checked for every case. Encoding proves this crate
//! produces the agreed bytes; decoding proves it accepts them and recovers the
//! same values. A conformance suite that only encoded would miss a decoder that
//! is wrong in a way its own encoder compensates for, which is exactly the bug
//! shape that survives to production and breaks the *other* client.
//!
//! The `invalid` sections matter as much as the happy path. A decoder that
//! accepts a 4 GiB length prefix is a remote out-of-memory primitive, and no
//! amount of round-tripping valid frames will find it.

use std::path::PathBuf;

use bytes::Bytes;
use migo_core::{Id, Timestamp};
use migo_wire::error::WireError;
use migo_wire::frame::{Fragment, FrameHeader, TraceContext};
use migo_wire::{varint, Frame, Reader, Writer};
use serde_json::Value;

// --- loading ----------------------------------------------------------------

fn vectors_dir() -> PathBuf {
    // The crate lives at server/crates/migo-wire; the vectors are shared with
    // every other language binding, so they sit above `server` and not inside it.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../shared/protocol/vectors/wire")
        .canonicalize()
        .expect(
            "shared/protocol/vectors/wire must exist; run tools/vectors/generate_wire_vectors.py",
        )
}

fn load(file: &str) -> Value {
    let path = vectors_dir().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Returns a section as a non-empty array.
///
/// Empty is a failure rather than a skip. A vector suite that silently runs zero
/// cases is the most expensive kind of green build: it reports coverage it does
/// not have, and it does so for as long as nobody looks.
fn section<'a>(file: &'a Value, name: &str, path: &str) -> &'a Vec<Value> {
    let array = file
        .get(name)
        .unwrap_or_else(|| panic!("{path} has no `{name}` section"))
        .as_array()
        .unwrap_or_else(|| panic!("{path} `{name}` is not an array"));
    assert!(!array.is_empty(), "{path} `{name}` is empty");
    array
}

// --- JSON field accessors ---------------------------------------------------

fn text<'a>(case: &'a Value, key: &str) -> &'a str {
    case.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case is missing string field `{key}`: {case}"))
}

fn name(case: &Value) -> &str {
    text(case, "name")
}

fn bytes_of(case: &Value, key: &str) -> Vec<u8> {
    hex::decode(text(case, key)).unwrap_or_else(|e| panic!("field `{key}` is not hex: {e}"))
}

/// Reads an integer that the file writes as a decimal string.
///
/// JSON numbers are doubles in JavaScript, so `18446744073709551615` would not
/// survive a round trip through the TypeScript runner. Quoting them keeps one
/// file readable by both languages.
fn integer(case: &Value, key: &str) -> u64 {
    let raw = case
        .get(key)
        .unwrap_or_else(|| panic!("case is missing field `{key}`: {case}"));
    match raw {
        Value::String(text) => text
            .parse()
            .unwrap_or_else(|e| panic!("field `{key}` is not an integer: {e}")),
        Value::Number(number) => number
            .as_u64()
            .unwrap_or_else(|| panic!("field `{key}` is not an unsigned integer")),
        other => panic!("field `{key}` has type {other:?}"),
    }
}

fn small(case: &Value, key: &str) -> u32 {
    u32::try_from(integer(case, key)).expect("value fits u32")
}

fn id_of(hex_text: &str) -> Id {
    let raw = hex::decode(hex_text).expect("id is hex");
    let raw: [u8; 16] = raw.try_into().expect("an id is exactly 16 bytes");
    Id::from_bytes(raw)
}

/// The variant name of an error, which is what a vector file names.
///
/// A file cannot depend on a `Display` string: those exist for operators and are
/// reworded freely. The variant name is the stable identity, and every language
/// binding can produce the same token for its own error type.
fn kind(error: &WireError) -> &'static str {
    match error {
        WireError::UnexpectedEnd { .. } => "UnexpectedEnd",
        WireError::VarintTooLong { .. } => "VarintTooLong",
        WireError::NonMinimalVarint { .. } => "NonMinimalVarint",
        WireError::StringTooLong { .. } => "StringTooLong",
        WireError::BytesTooLong { .. } => "BytesTooLong",
        WireError::ListTooLong { .. } => "ListTooLong",
        WireError::DepthExceeded { .. } => "DepthExceeded",
        WireError::FrameTooLarge { .. } => "FrameTooLarge",
        WireError::InvalidUtf8 => "InvalidUtf8",
        WireError::InvalidBool { .. } => "InvalidBool",
        WireError::UnsupportedVersion { .. } => "UnsupportedVersion",
        WireError::ReservedFlags { .. } => "ReservedFlags",
        WireError::TrailingBytes { .. } => "TrailingBytes",
        WireError::BatchTooLarge { .. } => "BatchTooLarge",
        WireError::NestedBatch => "NestedBatch",
        WireError::DecompressFailed => "DecompressFailed",
        WireError::DecompressedTooLarge { .. } => "DecompressedTooLarge",
        WireError::LengthOverflow { .. } => "LengthOverflow",
        WireError::InvalidFragment { .. } => "InvalidFragment",
        WireError::FieldOverflow { .. } => "FieldOverflow",
    }
}

/// Asserts that `result` failed with the error the case names, and says which
/// case when it did not — a bare `assert!(result.is_err())` in a loop over
/// forty cases is a test that tells you nothing when it breaks.
fn expect_error<T: std::fmt::Debug>(case: &Value, result: Result<T, WireError>, context: &str) {
    let expected = text(case, "error");
    let why = case.get("why").and_then(Value::as_str).unwrap_or("");
    match result {
        Ok(value) => panic!(
            "{context} `{}` was accepted but must fail with {expected}: {why}\n  decoded: {value:?}",
            name(case)
        ),
        Err(error) => assert_eq!(
            kind(&error),
            expected,
            "{context} `{}` failed with the wrong error: {why}",
            name(case)
        ),
    }
}

// --- varint -----------------------------------------------------------------

#[test]
fn varints_encode_and_decode_as_the_vectors_say() {
    let file = load("varint.json");
    for case in section(&file, "cases", "varint.json") {
        let value = integer(case, "value");
        let expected = bytes_of(case, "hex");

        let mut encoded = Vec::new();
        varint::encode_u64(value, &mut encoded);
        assert_eq!(
            hex::encode(&encoded),
            hex::encode(&expected),
            "encoding {value} for case `{}`",
            name(case)
        );
        assert_eq!(
            varint::encoded_len(value),
            expected.len(),
            "predicted length for case `{}`",
            name(case)
        );

        let (decoded, used) = varint::decode_u64(&expected, 0)
            .unwrap_or_else(|e| panic!("case `{}` must decode: {e}", name(case)));
        assert_eq!(decoded, value, "decoding case `{}`", name(case));
        assert_eq!(used, expected.len(), "bytes consumed by `{}`", name(case));
    }
}

#[test]
fn the_zigzag_mapping_matches_the_vectors() {
    let file = load("varint.json");
    for case in section(&file, "zigzag", "varint.json") {
        let signed: i64 = text(case, "value").parse().expect("signed value");
        let encoded = integer(case, "encoded");
        assert_eq!(
            varint::zigzag_encode(signed),
            encoded,
            "zigzag_encode for case `{}`",
            name(case)
        );
        assert_eq!(
            varint::zigzag_decode(encoded),
            signed,
            "zigzag_decode for case `{}`",
            name(case)
        );
    }
}

#[test]
fn malformed_varints_are_rejected() {
    let file = load("varint.json");
    for case in section(&file, "invalid", "varint.json") {
        let input = bytes_of(case, "hex");
        expect_error(case, varint::decode_u64(&input, 0), "varint");
    }
}

// --- frames -----------------------------------------------------------------

fn header_from_case(frame: &Value) -> FrameHeader {
    let trace = frame.get("trace").filter(|v| !v.is_null()).map(|trace| {
        let trace_id = hex::decode(text(trace, "trace_id")).expect("trace id is hex");
        let span_id = hex::decode(text(trace, "span_id")).expect("span id is hex");
        TraceContext {
            trace_id: trace_id.try_into().expect("a trace id is 16 bytes"),
            span_id: span_id.try_into().expect("a span id is 8 bytes"),
        }
    });
    let fragment = frame
        .get("fragment")
        .filter(|v| !v.is_null())
        .map(|fragment| Fragment {
            index: small(fragment, "index"),
            total: small(fragment, "total"),
        });

    FrameHeader {
        version: small(frame, "version") as u8,
        flags: small(frame, "flags") as u8,
        opcode: small(frame, "opcode"),
        correlation: small(frame, "correlation"),
        trace,
        fragment,
    }
}

#[test]
fn frames_encode_and_decode_as_the_vectors_say() {
    let file = load("frames.json");
    for case in section(&file, "cases", "frames.json") {
        let spec = case.get("frame").expect("case has a frame");
        let expected = bytes_of(case, "hex");
        let header = header_from_case(spec);
        let payload = Bytes::from(bytes_of(spec, "payload"));

        let frame = Frame::new(header, payload.clone());
        let encoded = frame
            .encode()
            .unwrap_or_else(|e| panic!("case `{}` must encode: {e}", name(case)));
        assert_eq!(
            hex::encode(&encoded),
            hex::encode(&expected),
            "encoding case `{}`",
            name(case)
        );
        assert_eq!(
            frame.encoded_len(),
            expected.len(),
            "predicted length for case `{}`",
            name(case)
        );

        let decoded = Frame::decode(Bytes::from(expected.clone()))
            .unwrap_or_else(|e| panic!("case `{}` must decode: {e}", name(case)));
        assert_eq!(
            decoded.header,
            header,
            "decoded header for case `{}`",
            name(case)
        );
        assert_eq!(
            hex::encode(&decoded.payload),
            hex::encode(&payload),
            "decoded payload for case `{}`",
            name(case)
        );
    }
}

#[test]
fn length_prefixed_frames_match_the_vectors() {
    let file = load("frames.json");
    for case in section(&file, "length_prefixed", "frames.json") {
        let body = bytes_of(case, "frame_hex");
        let expected = bytes_of(case, "hex");

        let frame = Frame::decode(Bytes::from(body.clone()))
            .unwrap_or_else(|e| panic!("case `{}` must decode: {e}", name(case)));
        let encoded = frame
            .encode_length_prefixed()
            .unwrap_or_else(|e| panic!("case `{}` must encode: {e}", name(case)));
        assert_eq!(
            hex::encode(&encoded),
            hex::encode(&expected),
            "length-prefixed encoding of case `{}`",
            name(case)
        );

        let buffer = Bytes::from(expected);
        let (parsed, consumed) = Frame::decode_length_prefixed(&buffer)
            .unwrap_or_else(|e| panic!("case `{}` must parse: {e}", name(case)))
            .unwrap_or_else(|| panic!("case `{}` must be a complete frame", name(case)));
        assert_eq!(consumed, buffer.len(), "case `{}` consumed", name(case));
        assert_eq!(parsed, frame, "case `{}` round trip", name(case));
    }
}

#[test]
fn malformed_frames_are_rejected() {
    let file = load("frames.json");
    for case in section(&file, "invalid", "frames.json") {
        let input = Bytes::from(bytes_of(case, "hex"));
        expect_error(case, Frame::decode(input), "frame");
    }
}

// --- MSE --------------------------------------------------------------------

/// Replays a writer program from a vector file.
///
/// One interpreter covers every struct shape in the schema, which is why the
/// vectors describe programs rather than named types: neither this runner nor the
/// TypeScript one has to be regenerated when a struct is added.
fn write_ops(writer: &mut Writer, ops: &[Value]) -> Result<(), WireError> {
    for op in ops {
        match text(op, "op") {
            "enter" => writer.enter()?,
            "leave" => writer.leave(),
            "bool" => writer.write_bool(op["value"].as_bool().expect("bool value")),
            "u32" => writer.write_u32(small(op, "value")),
            "u64" => writer.write_u64(integer(op, "value")),
            "timestamp" => writer.write_timestamp(Timestamp::from_wire(integer(op, "value"))),
            "id" => writer.write_id(&id_of(text(op, "value"))),
            "string" => writer.write_str(text(op, "value"))?,
            "bytes" => writer.write_bytes(&bytes_of(op, "value"))?,
            "list_len" => writer.list_len(integer(op, "value") as usize)?,
            "optional" => {
                let field_id = small(op, "id");
                let inner = op["ops"].as_array().expect("optional has ops").clone();
                writer.optional(field_id, |writer| write_ops(writer, &inner))?;
            }
            other => panic!("unknown write op `{other}`"),
        }
    }
    Ok(())
}

/// Replays the same program as reads.
///
/// An op that carries a `value` is asserted against it; an op without one is
/// read and discarded, which is what the malformed-input cases need. An op marked
/// `unknown` is the forward-compatibility path: the field is skipped by its
/// length instead of being decoded, exactly as a generated decoder does with a
/// field id from a newer peer.
fn read_ops(reader: &mut Reader, ops: &[Value], case: &str) -> Result<(), WireError> {
    for op in ops {
        match text(op, "op") {
            "enter" => reader.enter()?,
            "leave" => reader.leave(),
            "bool" => {
                let got = reader.read_bool()?;
                if let Some(expected) = op.get("value").and_then(Value::as_bool) {
                    assert_eq!(got, expected, "bool in case `{case}`");
                }
            }
            "u32" => {
                let got = reader.read_u32()?;
                if op.get("value").is_some() {
                    assert_eq!(got, small(op, "value"), "u32 in case `{case}`");
                }
            }
            "u64" => {
                let got = reader.read_u64()?;
                if op.get("value").is_some() {
                    assert_eq!(got, integer(op, "value"), "u64 in case `{case}`");
                }
            }
            "timestamp" => {
                let got = reader.read_timestamp()?;
                if op.get("value").is_some() {
                    assert_eq!(
                        got.to_wire(),
                        integer(op, "value"),
                        "timestamp in case `{case}`"
                    );
                }
            }
            "id" => {
                let got = reader.read_id()?;
                if let Some(expected) = op.get("value").and_then(Value::as_str) {
                    assert_eq!(got, id_of(expected), "id in case `{case}`");
                }
            }
            "string" => {
                let got = reader.read_string()?;
                if let Some(expected) = op.get("value").and_then(Value::as_str) {
                    assert_eq!(got, expected, "string in case `{case}`");
                }
            }
            "bytes" => {
                let got = reader.read_bytes()?;
                if op.get("value").is_some() {
                    assert_eq!(
                        hex::encode(&got),
                        text(op, "value"),
                        "bytes in case `{case}`"
                    );
                }
            }
            "list_len" => {
                let got = reader.read_list_len()?;
                if op.get("value").is_some() {
                    assert_eq!(
                        got as u64,
                        integer(op, "value"),
                        "list_len in case `{case}`"
                    );
                }
            }
            "optional" => {
                let (field_id, mut inner) = reader.read_optional()?;
                if op.get("id").is_some() {
                    assert_eq!(field_id, small(op, "id"), "field id in case `{case}`");
                }
                let unknown = op.get("unknown").and_then(Value::as_bool).unwrap_or(false);
                match op.get("ops").and_then(Value::as_array) {
                    // An unknown field is dropped with its sub-reader. That the
                    // outer position is already past it is the property being
                    // tested, and it is checked by the outer `finish`.
                    Some(_) if unknown => {}
                    Some(nested) => {
                        read_ops(&mut inner, nested, case)?;
                        inner.finish()?;
                    }
                    None => {}
                }
            }
            other => panic!("unknown read op `{other}`"),
        }
    }
    Ok(())
}

#[test]
fn mse_programs_encode_and_decode_as_the_vectors_say() {
    let file = load("mse.json");
    for case in section(&file, "cases", "mse.json") {
        let ops = case["ops"].as_array().expect("case has ops");
        let expected = bytes_of(case, "hex");

        let mut writer = Writer::new();
        write_ops(&mut writer, ops)
            .unwrap_or_else(|e| panic!("case `{}` must encode: {e}", name(case)));
        let encoded = writer
            .finish()
            .unwrap_or_else(|e| panic!("case `{}` must finish: {e}", name(case)));
        assert_eq!(
            hex::encode(&encoded),
            hex::encode(&expected),
            "encoding case `{}`",
            name(case)
        );

        let mut reader = Reader::new(Bytes::from(expected));
        read_ops(&mut reader, ops, name(case))
            .unwrap_or_else(|e| panic!("case `{}` must decode: {e}", name(case)));
        reader
            .finish()
            .unwrap_or_else(|e| panic!("case `{}` left bytes unread: {e}", name(case)));
    }
}

#[test]
fn malformed_mse_is_rejected() {
    let file = load("mse.json");
    for case in section(&file, "invalid", "mse.json") {
        let input = Bytes::from(bytes_of(case, "hex"));
        let ops = case["read_ops"].as_array().expect("case has read_ops");
        let mut reader = Reader::new(input);
        let result = read_ops(&mut reader, ops, name(case)).and_then(|()| reader.finish());
        expect_error(case, result, "mse");
    }
}

// --- the suite is present at all --------------------------------------------

#[test]
fn every_vector_file_is_present_and_populated() {
    // Guards against the failure this whole directory exists to prevent being
    // itself defeated by a missing file: without this, deleting `mse.json` turns
    // three tests into three panics that a `--no-fail-fast` run could bury, and
    // renaming a section turns them into silent no-ops.
    let expected: [(&str, &[&str]); 3] = [
        ("varint.json", &["cases", "zigzag", "invalid"]),
        ("frames.json", &["cases", "length_prefixed", "invalid"]),
        ("mse.json", &["cases", "invalid"]),
    ];
    let mut total = 0;
    for (file, sections) in expected {
        let loaded = load(file);
        assert!(
            loaded.get("provenance").and_then(Value::as_str).is_some(),
            "{file} must record where its expected bytes came from"
        );
        for name in sections {
            total += section(&loaded, name, file).len();
        }
    }
    assert!(
        total >= 60,
        "only {total} wire vector cases, expected at least 60"
    );
}
