//! Binary formats shown as JSON, read-only: MessagePack, and Protobuf without its schema. The
//! JSON text is what the Tree and Source views show; the file's bytes are never changed.

mod json_out;
pub mod msgpack;
pub mod protobuf;

/// Nesting and node limits, the same as the JSON tree's.
const MAX_NESTING: usize = 256;
const MAX_NODES: usize = 4_000_000;

/// Whether files of `format` are shown through a decoder.
pub fn supports(format: &str) -> bool {
    matches!(format, "msgpack" | "protobuf")
}

/// The JSON text for the bytes of a file in `format`, and a note on how it was shown; `None`
/// for formats without a decoder.
pub fn to_json(
    format: &str,
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<(String, String), String>> {
    match format {
        "msgpack" => Some(msgpack::to_json(bytes, cancelled)),
        "protobuf" => Some(protobuf::to_json(bytes, cancelled)),
        _ => None,
    }
}
