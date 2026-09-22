//! Every payload here was captured verbatim from the live API on 2026-09-22.
//! None is invented, and none comes from the docs — the docs do not describe
//! the error body at all.

use jevai::{ApiError, ApiStatus, ErrorDetail, ErrorType, Json};

fn parse(body: &str) -> ApiError {
    serde_json::from_str(body).expect("parse error body")
}

#[test]
fn object_shaped_detail_parses_with_a_machine_readable_type() {
    let err = parse(
        r#"{"detail":{"error_type":"authentication_error",
             "message":"Cannot authenticate with the server. Please check your API key and try again."}}"#,
    );

    assert_eq!(err.error_type(), Some(&ErrorType::AuthenticationError));
    assert!(err.is_auth());
    assert!(err.to_string().starts_with("authentication_error: Cannot authenticate"));

    let usage = parse(r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: gpt-4"}}"#);
    assert_eq!(usage.error_type(), Some(&ErrorType::ApiUsageError));
    assert!(!usage.is_auth());
    assert_eq!(usage.to_string(), "api_usage_error: Unknown model: gpt-4");
}

#[test]
fn string_shaped_detail_parses_as_prose() {
    for (body, expected) in [
        (
            r#"{"detail":"Noul question must have criteria or instructions: q"}"#,
            "Noul question must have criteria or instructions: q",
        ),
        (
            r#"{"detail":"Too many score levels. Must have at most 10 levels."}"#,
            "Too many score levels. Must have at most 10 levels.",
        ),
        (
            r#"{"detail":"Too many choices. Must have at most 255 choices."}"#,
            "Too many choices. Must have at most 255 choices.",
        ),
        (
            r#"{"detail":"Choice question must have at least one choice: q"}"#,
            "Choice question must have at least one choice: q",
        ),
    ] {
        let err = parse(body);
        assert!(matches!(err.detail, ErrorDetail::Message(_)), "{body}");
        assert_eq!(err.to_string(), expected);
        assert_eq!(err.error_type(), None, "prose carries no machine-readable type");
    }
}

#[test]
fn array_shaped_detail_parses_as_field_validation() {
    let err = parse(
        r#"{"detail":[{"type":"missing","loc":["body","state"],"msg":"Field required",
             "input":{"questions":{"q":{"type":"noul","instructions":"y?","criteria":null}},
                      "model":"jev-latest"}}]}"#,
    );

    let ErrorDetail::Validation(errors) = &err.detail else {
        panic!("expected the validation shape, got {:?}", err.detail);
    };
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, "missing");
    assert_eq!(errors[0].path(), "body.state");
    assert_eq!(errors[0].msg, "Field required");
    assert!(errors[0].input.is_some(), "the server echoes the rejected input");

    assert_eq!(err.to_string(), "body.state (missing): Field required");
    assert_eq!(
        err.invalid_fields().count(),
        1,
        "invalid_fields only yields entries for the validation shape"
    );
}

#[test]
fn validation_context_and_numeric_loc_entries_survive() {
    let err = parse(
        r#"{"detail":[{"type":"too_short","loc":["body","questions"],
             "msg":"Dictionary should have at least 1 item after validation, not 0","input":{},
             "ctx":{"field_type":"Dictionary","min_length":1,"actual_length":0}}]}"#,
    );
    let ErrorDetail::Validation(errors) = &err.detail else { panic!("wrong shape") };
    let ctx = errors[0].ctx.as_ref().expect("ctx present");
    assert_eq!(ctx.get("min_length"), Some(&Json::Int(1)));
    assert_eq!(ctx.get("field_type"), Some(&Json::Str("Dictionary".into())));

    // A JSON syntax error reports a byte offset, so `loc` mixes strings and
    // integers. `Vec<Json>` holds both because `Json` is itself untagged.
    let syntax = parse(
        r#"{"detail":[{"type":"json_invalid","loc":["body",9],"msg":"JSON decode error",
             "input":{},"ctx":{"error":"Expecting value"}}]}"#,
    );
    let ErrorDetail::Validation(errors) = &syntax.detail else { panic!("wrong shape") };
    assert_eq!(errors[0].loc, vec![Json::Str("body".into()), Json::Int(9)]);
    assert_eq!(errors[0].path(), "body.Int(9)");
}

#[test]
fn the_three_shapes_never_collide() {
    // Disjoint by JSON type, so untagged resolution is unambiguous in both
    // directions: each parses to its own variant and re-serializes unchanged.
    let cases = [
        r#"{"detail":"plain prose"}"#,
        r#"{"detail":{"error_type":"api_usage_error","message":"m"}}"#,
        r#"{"detail":[{"type":"missing","loc":["body","state"],"msg":"Field required"}]}"#,
    ];
    let variants: Vec<_> = cases
        .iter()
        .map(|body| {
            let err = parse(body);
            let round_tripped = serde_json::to_value(&err).unwrap();
            assert_eq!(round_tripped, serde_json::from_str::<serde_json::Value>(body).unwrap());
            std::mem::discriminant(&err.detail)
        })
        .collect();

    assert_ne!(variants[0], variants[1]);
    assert_ne!(variants[1], variants[2]);
    assert_ne!(variants[0], variants[2]);
}

#[test]
fn unknown_error_types_fall_back_instead_of_failing() {
    // Failing to parse an error response is the worst moment to start failing.
    let err = parse(r#"{"detail":{"error_type":"quota_exhausted_error","message":"nope"}}"#);
    assert_eq!(
        err.error_type(),
        Some(&ErrorType::Other("quota_exhausted_error".into()))
    );
    assert_eq!(err.to_string(), "quota_exhausted_error: nope");
    assert_eq!(
        serde_json::to_value(&err).unwrap()["detail"]["error_type"],
        serde_json::json!("quota_exhausted_error"),
        "the unknown name must survive a round trip verbatim"
    );
}

#[test]
fn status_codes_classify_retryability() {
    assert_eq!(ApiStatus::from_code(400), ApiStatus::BadRequest);
    assert_eq!(ApiStatus::from_code(401), ApiStatus::Unauthorized);
    assert_eq!(ApiStatus::from_code(422), ApiStatus::UnprocessableEntity);
    assert_eq!(ApiStatus::from_code(429), ApiStatus::RateLimited);
    assert_eq!(ApiStatus::from_code(529), ApiStatus::Overloaded);
    assert_eq!(ApiStatus::from_code(418), ApiStatus::Other(418));

    for retryable in [429, 529] {
        assert!(ApiStatus::from_code(retryable).is_retryable(), "{retryable}");
    }
    for fatal in [400, 401, 422, 418] {
        assert!(!ApiStatus::from_code(fatal).is_retryable(), "{fatal}");
    }
    assert_eq!(ApiStatus::from_code(429).code(), 429);
}
