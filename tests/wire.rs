//! Round-trip tests against the exact payloads printed in the API reference
//! (`.dev/doc.api.jev/api.md`). Comparison is on `serde_json::Value`, not on
//! strings, because JSON object order is not significant to the server.

use jevai::{
    Answer, ChoiceCriteria, InvalidCriteria, Json, Map, Model, NoulCriteria, Question,
    QuestionKind, Request, Response, ScoreCriteria, MAX_CHOICE_OPTIONS, MAX_SCORE_LEVELS,
};
use serde_json::json;

const STATE: &str = "Help! My payouts have been failing for 3 days.";

fn round_trips<T>(value: &T, expected: serde_json::Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(value).expect("serialize");
    assert_eq!(encoded, expected, "wire form differs from the documented one");

    // Round-trip through *text*, not through `serde_json::Value`. A `Value`
    // stores objects in a sorted `BTreeMap` unless serde_json is built with
    // `preserve_order`, so decoding from one would silently reorder `criteria`
    // and make this assertion test serde_json rather than this crate.
    let text = serde_json::to_string(value).expect("serialize to text");
    let decoded: T = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(&decoded, value, "value changed across a round trip");
}

#[test]
fn noul_request_matches_the_documented_shape() {
    let request = Request::new(STATE).ask(
        "is_urgent",
        Question::noul("Does this convey urgency?").with_noul_criteria(NoulCriteria::new(
            "Explicitly time-sensitive",
            "No urgency expressed",
        )),
    );

    round_trips(
        &request,
        json!({
            "state": STATE,
            "model": "jev-latest",
            "questions": {
                "is_urgent": {
                    "type": "noul",
                    "instructions": "Does this convey urgency?",
                    "criteria": {
                        "true": "Explicitly time-sensitive",
                        "false": "No urgency expressed"
                    }
                }
            }
        }),
    );
}

#[test]
fn a_noul_without_criteria_omits_the_field_entirely() {
    let request = Request::new(STATE).ask("is_urgent", Question::noul("Does this convey urgency?"));

    round_trips(
        &request,
        json!({
            "state": STATE,
            "model": "jev-latest",
            "questions": {
                "is_urgent": { "type": "noul", "instructions": "Does this convey urgency?" }
            }
        }),
    );
}

#[test]
fn choice_request_matches_the_documented_shape() {
    let request = Request::new(STATE).ask(
        "department",
        Question::choice(
            "Which team should handle this?",
            Map::new()
                .with("billing", "Payments, invoicing, refunds")
                .with("technical", "Bugs, outages, integrations")
                .with("sales", "Pricing, upgrades, new accounts"),
        )
        .expect("three options is valid"),
    );

    round_trips(
        &request,
        json!({
            "state": STATE,
            "model": "jev-latest",
            "questions": {
                "department": {
                    "type": "choice",
                    "instructions": "Which team should handle this?",
                    "criteria": {
                        "billing": "Payments, invoicing, refunds",
                        "technical": "Bugs, outages, integrations",
                        "sales": "Pricing, upgrades, new accounts"
                    }
                }
            }
        }),
    );
}

#[test]
fn score_request_matches_the_documented_shape() {
    let request = Request::new(STATE).ask(
        "frustration",
        Question::score(
            "How frustrated is the customer?",
            ["Calm", "Frustrated", "Very angry"],
        )
        .expect("three levels is valid"),
    );

    round_trips(
        &request,
        json!({
            "state": STATE,
            "model": "jev-latest",
            "questions": {
                "frustration": {
                    "type": "score",
                    "instructions": "How frustrated is the customer?",
                    "criteria": ["Calm", "Frustrated", "Very angry"]
                }
            }
        }),
    );
}

#[test]
fn structured_instructions_survive_a_round_trip() {
    // From api.md: the question in one field, the data it refers to in others.
    let instructions = Json::Object(
        Map::new()
            .with(
                "potential_duplicate",
                Json::Object(
                    Map::new()
                        .with("name", Json::from("John Smith"))
                        .with("location", Json::from("Oakland, California"))
                        .with("last_employer", Json::from("Google")),
                ),
            )
            .with(
                "question",
                Json::from("Is the resume for the same person as `potential_duplicate`?"),
            ),
    );

    let question = Question::noul(instructions);
    let text = serde_json::to_string(&question).unwrap();
    assert_eq!(
        text,
        r#"{"type":"noul","instructions":{"potential_duplicate":{"name":"John Smith","location":"Oakland, California","last_employer":"Google"},"question":"Is the resume for the same person as `potential_duplicate`?"}}"#,
        "nested object key order must survive verbatim"
    );
    assert_eq!(serde_json::from_str::<Question>(&text).unwrap(), question);
}

#[test]
fn noul_response_parses_and_exposes_its_value() {
    let body = json!({
        "model": "jev-1.13.0",
        "answers": { "is_urgent": { "type": "noul", "noul": 0.95 } },
        "usage": { "input_tokens": 307, "output_tokens": 20 }
    });

    let response: Response = serde_json::from_value(body.clone()).expect("parse");
    let answer = response.answer("is_urgent").expect("answer present");

    let noul = answer.as_noul().expect("a noul answer");
    assert_eq!(noul.noul, 0.95);
    assert!(noul.is_yes(0.9));
    assert!((noul.certainty() - 0.9).abs() < 1e-9);
    assert_eq!(answer.confidence(), None, "nouls carry no confidence field");
    assert_eq!(response.usage.input_tokens, 307);

    round_trips(&response, body);
}

#[test]
fn choice_response_parses_and_exposes_its_distribution() {
    let body = json!({
        "model": "jev-1.13.0",
        "answers": {
            "department": {
                "type": "choice",
                "choice": "billing",
                "probabilities": { "billing": 0.88, "technical": 0.12, "sales": 0.0 },
                "confidence": 0.81
            }
        },
        "usage": { "input_tokens": 318, "output_tokens": 34 }
    });

    let response: Response = serde_json::from_str(
        r#"{
            "model": "jev-1.13.0",
            "answers": {
                "department": {
                    "type": "choice",
                    "choice": "billing",
                    "probabilities": { "billing": 0.88, "technical": 0.12, "sales": 0.0 },
                    "confidence": 0.81
                }
            },
            "usage": { "input_tokens": 318, "output_tokens": 34 }
        }"#,
    )
    .expect("parse");
    let choice = response
        .answer("department")
        .and_then(Answer::as_choice)
        .expect("a choice answer");

    assert_eq!(choice.choice, "billing");
    assert_eq!(choice.confidence, 0.81);
    assert_eq!(choice.probability_of("technical"), Some(0.12));
    assert_eq!(choice.probability_of("nonexistent"), None);
    // Whatever order the server sends, it survives parsing unchanged. Verified
    // against the live API, which does NOT echo the criteria order back — a
    // request of billing/technical/sales came back technical/sales/billing.
    assert_eq!(
        choice.probabilities.keys().collect::<Vec<_>>(),
        ["billing", "technical", "sales"],
        "probability order must match the order received on the wire"
    );

    round_trips(&response, body);
}

#[test]
fn score_response_parses_and_maps_back_to_its_legend() {
    let body = json!({
        "model": "jev-1.13.0",
        "answers": {
            "frustration": {
                "type": "score",
                "score": 1.05,
                "legend": { "0": "Calm", "1": "Frustrated", "2": "Very angry" },
                "probabilities": { "0": 0.0, "1": 0.95, "2": 0.05 },
                "confidence": 0.92
            }
        },
        "usage": { "input_tokens": 304, "output_tokens": 18 }
    });

    let response: Response = serde_json::from_value(body.clone()).expect("parse");
    let score = response
        .answer("frustration")
        .and_then(Answer::as_score)
        .expect("a score answer");

    assert_eq!(score.score, 1.05);
    assert_eq!(score.nearest_level(), 1);
    assert_eq!(score.nearest_label(), Some("Frustrated"));
    assert_eq!(score.label(2), Some("Very angry"));
    assert_eq!(score.label(9), None);

    round_trips(&response, body);
}

#[test]
fn model_aliases_serialize_bare_and_unknown_names_still_parse() {
    assert_eq!(serde_json::to_value(Model::JevLatest).unwrap(), json!("jev-latest"));
    assert_eq!(serde_json::to_value(Model::JevPreview).unwrap(), json!("jev-preview"));

    let pinned: Model = serde_json::from_value(json!("jev-1.13.0")).unwrap();
    assert_eq!(pinned, Model::Version("jev-1.13.0".into()));
    assert!(!pinned.is_alias());
    assert!(Model::JevLatest.is_alias());

    // A model released after this crate was built must not break parsing.
    let future: Model = serde_json::from_value(json!("jev-2.0.0")).unwrap();
    assert_eq!(future.as_str(), "jev-2.0.0");
    assert_eq!(serde_json::to_value(&future).unwrap(), json!("jev-2.0.0"));
}

#[test]
fn criteria_bounds_are_enforced_on_construction_and_on_parse() {
    assert_eq!(
        ChoiceCriteria::new(Map::new()),
        Err(InvalidCriteria::NoChoiceOptions)
    );

    let too_many: Map<Json> = (0..=MAX_CHOICE_OPTIONS)
        .map(|i| (i.to_string(), Json::Null))
        .collect();
    assert_eq!(
        ChoiceCriteria::new(too_many),
        Err(InvalidCriteria::TooManyChoiceOptions(MAX_CHOICE_OPTIONS + 1))
    );

    // One level is accepted: the live API answers it rather than rejecting it,
    // even though the docs recommend two.
    assert!(ScoreCriteria::new(vec![Json::from("only one")]).is_ok());
    assert_eq!(
        ScoreCriteria::new(Vec::new()),
        Err(InvalidCriteria::TooFewScoreLevels(0))
    );
    let eleven: Vec<Json> = (0..=MAX_SCORE_LEVELS).map(|i| Json::Int(i as i64)).collect();
    assert_eq!(
        ScoreCriteria::new(eleven),
        Err(InvalidCriteria::TooManyScoreLevels(MAX_SCORE_LEVELS + 1))
    );

    // The same bound applies through serde, so an invalid question cannot be
    // conjured by deserializing one.
    let err = serde_json::from_value::<Question>(json!({
        "type": "score",
        "instructions": "rate it",
        "criteria": []
    }))
    .unwrap_err();
    assert!(err.to_string().contains("below the minimum"), "{err}");
}

#[test]
fn question_kind_tag_matches_the_serialized_type_field() {
    let cases = [
        Question::noul("q"),
        Question::choice("q", Map::new().with("a", Json::Null)).unwrap(),
        Question::score("q", ["lo", "hi"]).unwrap(),
    ];

    for question in cases {
        let encoded = serde_json::to_value(&question).unwrap();
        assert_eq!(encoded["type"], json!(question.kind.tag()));
        assert!(
            encoded.get("kind").is_none(),
            "flatten must not leak a `kind` wrapper: {encoded}"
        );
    }
}

#[test]
fn map_preserves_insertion_order_across_a_round_trip() {
    let map: Map<Json> = Map::new()
        .with("zebra", Json::Int(1))
        .with("alpha", Json::Int(2))
        .with("middle", Json::Int(3));

    let encoded = serde_json::to_string(&map).unwrap();
    assert_eq!(encoded, r#"{"zebra":1,"alpha":2,"middle":3}"#);

    let decoded: Map<Json> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.keys().collect::<Vec<_>>(), ["zebra", "alpha", "middle"]);
}

#[test]
fn integers_do_not_become_floats_across_a_round_trip() {
    let value = Json::Object(
        Map::new()
            .with("count", Json::Int(3))
            .with("ratio", Json::Float(0.5))
            .with("flag", Json::Bool(true))
            .with("nothing", Json::Null),
    );

    let encoded = serde_json::to_string(&value).unwrap();
    assert_eq!(encoded, r#"{"count":3,"ratio":0.5,"flag":true,"nothing":null}"#);
    assert_eq!(serde_json::from_str::<Json>(&encoded).unwrap(), value);
}

#[test]
fn json_bridges_to_and_from_serde_value() {
    let original = json!({ "a": [1, 2.5, "x", null, true] });
    let bridged = Json::from_serde(&original);
    assert_eq!(bridged.to_serde(), original);

    assert_eq!(
        bridged
            .as_object()
            .and_then(|m| m.get("a"))
            .and_then(Json::as_array)
            .map(<[Json]>::len),
        Some(5)
    );
}

#[test]
fn unknown_question_type_is_rejected() {
    let err = serde_json::from_value::<Question>(json!({
        "type": "vibes",
        "instructions": "hmm"
    }))
    .unwrap_err();
    assert!(err.to_string().contains("unknown variant"), "{err}");
}

#[test]
fn a_full_multi_question_request_round_trips() {
    let request = Request::new(STATE)
        .with_model(Model::Version("jev-1.13.0".into()))
        .ask("is_urgent", Question::noul("Does this convey urgency?"))
        .ask(
            "department",
            Question::choice(
                "Which team?",
                Map::new().with("billing", Json::Null).with("technical", Json::Null),
            )
                .unwrap(),
        )
        .ask(
            "frustration",
            Question::score("How frustrated?", ["Calm", "Frustrated", "Very angry"]).unwrap(),
        );

    assert_eq!(request.questions.len(), 3);
    let encoded = serde_json::to_string(&request).unwrap();
    let decoded: Request = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, request);
    assert!(matches!(
        decoded.questions.get("frustration").map(|q| &q.kind),
        Some(QuestionKind::Score(_))
    ));
}

/// Responses captured verbatim from the live API on 2026-09-22, not copied from
/// the docs. The doc examples are idealized; these are what the server actually
/// sends — note that `severity` orders its fields `score, confidence, legend,
/// probabilities`, unlike the documented example.
#[test]
fn live_captured_responses_parse() {
    let simple: Response = serde_json::from_str(
        r#"{"model":"jev-1.13.0","answers":{"is_urgent":{"type":"noul","noul":0.95}},
            "usage":{"input_tokens":283,"output_tokens":23}}"#,
    )
    .expect("parse live noul response");
    assert_eq!(
        simple.answer("is_urgent").and_then(Answer::as_noul).map(|n| n.noul),
        Some(0.95)
    );

    let structured: Response = serde_json::from_str(
        r#"{"model":"jev-1.13.0","answers":{
              "same_person":{"type":"noul","noul":0.15},
              "severity":{"type":"score","score":2.45,"confidence":0.53,
                          "legend":{"0":"Trivial","1":"Minor","2":"Major","3":"Critical"},
                          "probabilities":{"0":0.0,"1":0.01,"2":0.53,"3":0.46}}},
            "usage":{"input_tokens":434,"output_tokens":34}}"#,
    )
    .expect("parse live structured response");

    let severity = structured
        .answer("severity")
        .and_then(Answer::as_score)
        .expect("a score answer");
    assert_eq!(severity.nearest_level(), 2);
    assert_eq!(severity.nearest_label(), Some("Major"));
    assert_eq!(severity.legend.len(), 4);

    // A four-level score is legal; the crate's bound is 2..=10.
    assert_eq!(
        structured.answer("same_person").and_then(Answer::as_noul).map(|n| n.noul),
        Some(0.15)
    );

    let choice_live: Response = serde_json::from_str(
        r#"{"model":"jev-1.13.0","answers":{"department":{"type":"choice","choice":"billing",
              "probabilities":{"technical":0.05,"sales":0.0,"billing":0.95},"confidence":0.93}},
            "usage":{"input_tokens":436,"output_tokens":73}}"#,
    )
    .expect("parse live choice response");
    let choice = choice_live
        .answer("department")
        .and_then(Answer::as_choice)
        .expect("a choice answer");
    assert_eq!(choice.choice, "billing");
    assert_eq!(choice.probability_of("billing"), Some(0.95));
    assert_eq!(
        choice.probabilities.keys().collect::<Vec<_>>(),
        ["technical", "sales", "billing"],
        "the server does not echo criteria order; whatever it sends is preserved"
    );
}

#[test]
fn the_responding_model_is_typed_and_always_concrete() {
    let response: Response = serde_json::from_str(
        r#"{"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.5}},
            "usage":{"input_tokens":1,"output_tokens":1}}"#,
    )
    .unwrap();

    // An alias was sent; a pinned version answered. The untagged fallback is
    // what lets an unrecognized version land here rather than fail the parse.
    assert_eq!(response.model, Model::Version("jev-1.13.0".into()));
    assert!(!response.model.is_alias());
    assert_eq!(response.model.to_string(), "jev-1.13.0");
}
