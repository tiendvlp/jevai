//! rkyv round-trips over the same message types serde handles.
//!
//! The point of carrying both: JSON talks to the API, rkyv caches the result.
//! Reading an archived response needs no parse step at all — the bytes are the
//! data structure.

#![cfg(feature = "rkyv")]

use jevai::answer::ArchivedAnswer;
use jevai::json::ArchivedJson;
use jevai::message::{ArchivedModel, ArchivedRequest, ArchivedResponse};
use jevai::rkyv::rancor::Error;
use jevai::{Json, Map, Model, NoulCriteria, Question, Request, Response};

fn sample_request() -> Request {
    Request::new("Help! My payouts have been failing for 3 days.")
        .with_model(Model::Version("jev-1.13.0".into()))
        .ask(
            "is_urgent",
            Question::noul("Does this convey urgency?").with_noul_criteria(NoulCriteria::new(
                "Explicitly time-sensitive",
                "No urgency expressed",
            )),
        )
        .ask(
            "department",
            Question::choice(
                "Which team should handle this?",
                Map::new()
                    .with("billing", "Payments, invoicing, refunds")
                    .with("technical", "Bugs, outages, integrations"),
            )
            .unwrap(),
        )
        .ask(
            "frustration",
            Question::score("How frustrated?", ["Calm", "Frustrated", "Very angry"]).unwrap(),
        )
}

fn sample_response() -> Response {
    serde_json::from_str(
        r#"{
            "model": "jev-1.13.0",
            "answers": {
                "is_urgent": { "type": "noul", "noul": 0.95 },
                "department": {
                    "type": "choice",
                    "choice": "billing",
                    "probabilities": { "billing": 0.88, "technical": 0.12 },
                    "confidence": 0.81
                },
                "frustration": {
                    "type": "score",
                    "score": 1.05,
                    "legend": { "0": "Calm", "1": "Frustrated", "2": "Very angry" },
                    "probabilities": { "0": 0.0, "1": 0.95, "2": 0.05 },
                    "confidence": 0.92
                }
            },
            "usage": { "input_tokens": 304, "output_tokens": 18 }
        }"#,
    )
    .unwrap()
}

#[test]
fn request_survives_an_archive_round_trip() {
    let request = sample_request();
    let bytes = jevai::rkyv::to_bytes::<Error>(&request).expect("archive");
    let restored: Request = jevai::rkyv::from_bytes::<Request, Error>(&bytes).expect("restore");
    assert_eq!(restored, request);
}

#[test]
fn response_survives_an_archive_round_trip() {
    let response = sample_response();
    let bytes = jevai::rkyv::to_bytes::<Error>(&response).expect("archive");
    let restored: Response = jevai::rkyv::from_bytes::<Response, Error>(&bytes).expect("restore");
    assert_eq!(restored, response);
}

#[test]
fn archived_response_is_readable_without_deserializing() {
    let bytes = jevai::rkyv::to_bytes::<Error>(&sample_response()).expect("archive");

    // No allocation, no parse: this borrows straight out of the buffer.
    let archived = jevai::rkyv::access::<ArchivedResponse, Error>(&bytes).expect("validate");

    assert_eq!(archived.model.as_str(), "jev-1.13.0");
    assert_eq!(archived.usage.input_tokens, 304);

    let urgent = archived.answers.get("is_urgent").expect("answer present");

    match urgent {
        ArchivedAnswer::Noul(noul) => assert!((noul.noul.to_native() - 0.95).abs() < 1e-9),
        other => panic!("expected a noul answer, got {other:?}"),
    }
}

#[test]
fn archived_request_preserves_question_order_and_model() {
    let bytes = jevai::rkyv::to_bytes::<Error>(&sample_request()).expect("archive");
    let archived = jevai::rkyv::access::<ArchivedRequest, Error>(&bytes).expect("validate");

    assert!(matches!(archived.model, ArchivedModel::Version(_)));
    assert_eq!(
        archived.questions.keys().collect::<Vec<_>>(),
        ["is_urgent", "department", "frustration"],
        "archived maps keep insertion order like their unarchived form"
    );
    assert!(matches!(archived.state, ArchivedJson::Str(_)));
}

#[test]
fn deeply_nested_json_archives() {
    // The recursive variants are the ones needing `omit_bounds`; exercise them.
    let nested = Json::Object(
        Map::new()
            .with(
                "list",
                Json::Array(vec![
                    Json::Int(1),
                    Json::Float(2.5),
                    Json::Array(vec![Json::Str("deep".into()), Json::Null, Json::Bool(true)]),
                ]),
            )
            .with("nested", Json::Object(Map::new().with("k", Json::Int(-7)))),
    );

    let bytes = jevai::rkyv::to_bytes::<Error>(&nested).expect("archive");
    let restored: Json = jevai::rkyv::from_bytes::<Json, Error>(&bytes).expect("restore");
    assert_eq!(restored, nested);
}

/// rkyv's archived format is controlled by *additive Cargo features*, so any
/// crate anywhere in the final binary can change it for everyone. These
/// assertions pin the defaults this crate's archives assume. If one fails,
/// something in the dependency graph enabled `big_endian`, `unaligned`, or a
/// non-default `pointer_width_*`, and previously written archives are no
/// longer readable.
#[test]
fn archived_format_stays_portable() {
    use jevai::rkyv::string::ArchivedString;
    use jevai::rkyv::vec::ArchivedVec;
    use jevai::Usage;

    // Little-endian, fixed by rkyv rather than inherited from the host, so an
    // archive written on x86 reads on a big-endian target.
    let usage = Usage {
        input_tokens: 0x0102_0304_0506_0708,
        output_tokens: 0,
    };
    let bytes = jevai::rkyv::to_bytes::<Error>(&usage).unwrap();
    assert_eq!(
        &bytes[..8],
        &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
        "multi-byte integers must be little-endian regardless of host"
    );

    // 32-bit relative pointers: an 8-byte ArchivedString/ArchivedVec on a
    // 64-bit host. Keeps archives readable on 32-bit targets, and caps any
    // single archive at ~2 GiB.
    assert_eq!(size_of::<ArchivedString>(), 8, "expected 32-bit relative pointers");
    assert_eq!(size_of::<ArchivedVec<u8>>(), 8, "expected 32-bit relative pointers");
}

#[test]
fn access_requires_an_aligned_buffer() {
    let bytes = jevai::rkyv::to_bytes::<Error>(&sample_response()).unwrap();
    assert!(
        jevai::rkyv::access::<ArchivedResponse, Error>(&bytes).is_ok(),
        "AlignedVec is always correctly aligned"
    );

    // The same bytes offset by one: this is what a naive `Vec<u8>` read from a
    // file, socket, or mmap can look like. Aligned primitives are the default,
    // so access refuses rather than reading garbage.
    let mut shifted = vec![0u8];
    shifted.extend_from_slice(&bytes);
    let err = jevai::rkyv::access::<ArchivedResponse, Error>(&shifted[1..])
        .expect_err("misaligned access must be rejected");
    assert!(
        err.to_string().contains("unaligned"),
        "expected an alignment error, got: {err}"
    );
}
