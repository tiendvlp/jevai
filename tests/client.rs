//! Client behaviour against a mock server: the head/body split, header
//! access, and error mapping. No network, no API key, no quota — so these
//! run in CI.

#![cfg(all(feature = "client", not(target_arch = "wasm32")))]

use std::time::Duration;

use jevai::client::Error;
use jevai::{Answer, JevClient, Question, Request};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OK_BODY: &str = r#"{"model":"jev-1.13.0",
    "answers":{"is_urgent":{"type":"noul","noul":0.95}},
    "usage":{"input_tokens":283,"output_tokens":23}}"#;

const PATH: &str = "/v1/systemone";

fn sample_request() -> Request {
    Request::new("Payouts failing for 3 days.")
        .ask("is_urgent", Question::noul("Does this convey urgency?"))
}

/// A client pointed at the mock.
fn client_for(server: &MockServer) -> JevClient {
    JevClient::builder("test-key")
        .endpoint(format!("{}{PATH}", server.uri()))
        .build()
        .expect("build client")
}

#[tokio::test]
async fn sends_the_expected_request_and_parses_the_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .and(header("authorization", "Bearer test-key"))
        .and(header("content-type", "application/json"))
        .and(body_string_contains("\"type\":\"noul\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(OK_BODY))
        .expect(1)
        .mount(&server)
        .await;

    let response = client_for(&server).ask(&sample_request()).await.unwrap();

    assert_eq!(response.model.as_str(), "jev-1.13.0");
    assert_eq!(
        response.answer("is_urgent").and_then(Answer::as_noul).map(|n| n.noul),
        Some(0.95)
    );
    assert_eq!(response.usage.input_tokens, 283);
}

#[tokio::test]
async fn the_head_carries_the_request_id_and_server_timing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-typesafe-request-id", "req_01a0c98b136c7054")
                .insert_header("x-envoy-upstream-service-time", "101")
                .set_body_string(OK_BODY),
        )
        .mount(&server)
        .await;

    let received = client_for(&server).send(&sample_request()).await.unwrap();

    // All of this is readable before the body has been touched.
    assert!(received.is_success());
    assert_eq!(received.status().code(), 200);
    assert_eq!(received.request_id(), Some("req_01a0c98b136c7054"));
    assert_eq!(received.server_time(), Some(Duration::from_millis(101)));
    assert!(received.headers().contains_key("content-type"));

    let response = received.json().await.unwrap();
    assert_eq!(response.model.as_str(), "jev-1.13.0");
}

#[tokio::test]
async fn a_missing_metadata_header_is_absence_not_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(OK_BODY))
        .mount(&server)
        .await;

    let received = client_for(&server).send(&sample_request()).await.unwrap();
    assert_eq!(received.request_id(), None);
    assert_eq!(received.server_time(), None);
    assert_eq!(received.retry_after(), None);
}

#[tokio::test]
async fn a_non_2xx_status_is_not_an_error_until_the_body_is_read() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"detail":"nope"}"#))
        .mount(&server)
        .await;

    // send() resolves on the head, which cannot explain the failure on its own.
    let received = client_for(&server).send(&sample_request()).await.unwrap();
    assert!(!received.is_success());
    assert_eq!(received.status().code(), 400);

    // Classification needs the body, so it happens here.
    assert!(matches!(received.json().await, Err(Error::Api { .. })));
}

#[tokio::test]
async fn nothing_is_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(529).set_body_string(r#"{"detail":"Overloaded. Try again."}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let err = client_for(&server).ask(&sample_request()).await.unwrap_err();

    let Error::Api { status, .. } = err else {
        panic!("expected Api, got {err:?}");
    };
    assert_eq!(status.code(), 529);
    assert!(status.is_retryable(), "the status is advice, not an action");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a retryable status must still be delivered exactly once"
    );
}

#[tokio::test]
async fn a_429_reports_the_delay_the_server_asked_for() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .set_body_string(r#"{"detail":{"error_type":"rate_limit_error","message":"slow down"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let received = client_for(&server).send(&sample_request()).await.unwrap();

    assert_eq!(received.status().code(), 429);
    assert!(received.status().is_retryable());
    assert_eq!(
        received.retry_after(),
        Some(Duration::from_secs(30)),
        "the caller needs this to back off itself"
    );
}

#[tokio::test]
async fn an_http_date_retry_after_reports_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")
                .set_body_string(r#"{"detail":"slow down"}"#),
        )
        .mount(&server)
        .await;

    // The date form needs a wall clock, which wasm does not have.
    let received = client_for(&server).send(&sample_request()).await.unwrap();
    assert_eq!(received.retry_after(), None);
}

#[tokio::test]
async fn maps_a_400_to_a_typed_usage_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: gpt-4"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let err = client_for(&server).ask(&sample_request()).await.unwrap_err();

    let Error::Api { status, error, .. } = err else {
        panic!("expected Api, got {err:?}");
    };
    assert_eq!(status.code(), 400);
    assert!(!status.is_retryable());
    assert_eq!(error.to_string(), "api_usage_error: Unknown model: gpt-4");
}

#[tokio::test]
async fn maps_a_401_to_a_typed_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(401).set_body_string(
            r#"{"detail":{"error_type":"authentication_error","message":"Cannot authenticate"}}"#,
        ))
        .mount(&server)
        .await;

    let Error::Api { status, error, .. } = client_for(&server)
        .ask(&sample_request())
        .await
        .unwrap_err()
    else {
        panic!("expected Api");
    };
    assert_eq!(status.code(), 401);
    assert!(error.is_auth());
}

#[tokio::test]
async fn maps_a_422_to_typed_field_validation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(422).set_body_string(
            r#"{"detail":[{"type":"missing","loc":["body","state"],"msg":"Field required"}]}"#,
        ))
        .mount(&server)
        .await;

    let Error::Api { status, error, .. } = client_for(&server)
        .ask(&sample_request())
        .await
        .unwrap_err()
    else {
        panic!("expected Api");
    };
    assert_eq!(status.code(), 422);
    assert_eq!(error.invalid_fields().count(), 1);
    assert_eq!(error.to_string(), "body.state (missing): Field required");
}

#[tokio::test]
async fn the_request_id_survives_into_the_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-typesafe-request-id", "req_deadbeef")
                .set_body_string(r#"{"detail":"nope"}"#),
        )
        .mount(&server)
        .await;

    // json() consumes the head, so the id has to be carried across.
    let err = client_for(&server).ask(&sample_request()).await.unwrap_err();
    assert_eq!(err.request_id(), Some("req_deadbeef"));
    assert_eq!(err.status().map(|s| s.code()), Some(400));
    assert!(err.to_string().contains("req_deadbeef"), "{err}");
}

#[tokio::test]
async fn keeps_the_body_of_an_unrecognized_error_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(502).set_body_string("<html><body>Bad Gateway</body></html>"),
        )
        .mount(&server)
        .await;

    let err = client_for(&server).ask(&sample_request()).await.unwrap_err();
    let Error::UnexpectedBody { status, body, .. } = err else {
        panic!("expected UnexpectedBody, got {err:?}");
    };
    assert_eq!(status.code(), 502);
    assert!(body.contains("Bad Gateway"), "the raw body must survive: {body}");
}

#[tokio::test]
async fn reports_an_undecodable_success_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"model":"jev-1.13.0"}"#))
        .mount(&server)
        .await;

    let err = client_for(&server).ask(&sample_request()).await.unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err:?}");
}

#[tokio::test]
async fn the_body_can_be_read_unparsed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(OK_BODY))
        .mount(&server)
        .await;

    let client = client_for(&server);

    let text = client.send(&sample_request()).await.unwrap().text().await.unwrap();
    assert!(text.contains("jev-1.13.0"));

    let bytes = client.send(&sample_request()).await.unwrap().bytes().await.unwrap();
    assert_eq!(bytes, text.as_bytes());
}

#[tokio::test]
async fn drops_to_the_underlying_reqwest_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(OK_BODY))
        .mount(&server)
        .await;

    // The escape hatch for anything this wrapper does not model.
    let raw = client_for(&server)
        .send(&sample_request())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(raw.status().as_u16(), 200);
}

#[tokio::test]
async fn errors_convert_into_anyhow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"detail":"nope"}"#))
        .mount(&server)
        .await;

    // The point of thiserror here: `?` into anyhow with no glue.
    async fn call(client: &JevClient, request: &Request) -> Result<(), Box<dyn std::error::Error>> {
        client.ask(request).await?;
        Ok(())
    }

    let err = call(&client_for(&server), &sample_request()).await.unwrap_err();
    assert!(err.to_string().contains("400"), "{err}");
}
