//! Client behaviour against a mock server: retries, error mapping, headers.
//! No network, no API key, no quota — so these run in CI.

#![cfg(all(feature = "client", not(target_arch = "wasm32")))]

use std::time::Duration;

use jevai::client::{Error, RetryPolicy};
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

/// A client pointed at the mock, with backoff collapsed so tests stay fast.
fn client_for(server: &MockServer, max_attempts: u32) -> JevClient {
    JevClient::builder("test-key")
        .endpoint(format!("{}{PATH}", server.uri()))
        .retry_policy(RetryPolicy {
            max_attempts,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            jitter: false,
            ..RetryPolicy::default()
        })
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

    let response = client_for(&server, 3).send(&sample_request()).await.unwrap();

    assert_eq!(response.model.as_str(), "jev-1.13.0");
    assert_eq!(
        response.answer("is_urgent").and_then(Answer::as_noul).map(|n| n.noul),
        Some(0.95)
    );
    assert_eq!(response.usage.input_tokens, 283);
}

#[tokio::test]
async fn retries_a_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(429).set_body_string(
            r#"{"detail":{"error_type":"rate_limit_error","message":"slow down"}}"#,
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(OK_BODY))
        .with_priority(2)
        .mount(&server)
        .await;

    let response = client_for(&server, 3).send(&sample_request()).await.unwrap();
    assert_eq!(response.model.as_str(), "jev-1.13.0");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn retries_a_529_then_gives_up_with_the_last_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(529).set_body_string(r#"{"detail":"Overloaded. Try again."}"#),
        )
        .mount(&server)
        .await;

    let err = client_for(&server, 3).send(&sample_request()).await.unwrap_err();

    let Error::RetriesExhausted { attempts, last } = err else {
        panic!("expected RetriesExhausted, got {err:?}");
    };
    assert_eq!(attempts, 3);
    assert!(matches!(*last, Error::Api { .. }), "{last:?}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "max_attempts counts the first try"
    );
}

#[tokio::test]
async fn honors_retry_after_seconds_over_computed_backoff() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "1")
                .set_body_string(r#"{"detail":"slow down"}"#),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(OK_BODY))
        .with_priority(2)
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    client_for(&server, 3).send(&sample_request()).await.unwrap();

    // Configured backoff is 1ms; the header asks for 1s and must win.
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "retry-after was ignored: waited {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn does_not_retry_a_400() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: gpt-4"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let err = client_for(&server, 5).send(&sample_request()).await.unwrap_err();

    let Error::Api { status, error } = err else {
        panic!("expected Api, got {err:?}");
    };
    assert_eq!(status.code(), 400);
    assert!(!status.is_retryable());
    assert_eq!(error.to_string(), "api_usage_error: Unknown model: gpt-4");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a deterministic rejection must not be retried"
    );
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

    let Error::Api { status, error } = client_for(&server, 3)
        .send(&sample_request())
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

    let Error::Api { status, error } = client_for(&server, 3)
        .send(&sample_request())
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
async fn keeps_the_body_of_an_unrecognized_error_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(
            ResponseTemplate::new(502).set_body_string("<html><body>Bad Gateway</body></html>"),
        )
        .mount(&server)
        .await;

    let err = client_for(&server, 1).send(&sample_request()).await.unwrap_err();
    let Error::UnexpectedBody { status, body } = err else {
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

    let err = client_for(&server, 3).send(&sample_request()).await.unwrap_err();
    assert!(matches!(err, Error::Decode(_)), "{err:?}");
}

#[tokio::test]
async fn a_single_attempt_policy_returns_the_error_directly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH))
        .respond_with(ResponseTemplate::new(529).set_body_string(r#"{"detail":"Overloaded"}"#))
        .expect(1)
        .mount(&server)
        .await;

    // With one attempt there is no retry sequence, so the error is not wrapped.
    let err = client_for(&server, 1).send(&sample_request()).await.unwrap_err();
    assert!(matches!(err, Error::RetriesExhausted { attempts: 1, .. }), "{err:?}");
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
        client.send(request).await?;
        Ok(())
    }

    let err = call(&client_for(&server, 1), &sample_request()).await.unwrap_err();
    assert!(err.to_string().contains("400"), "{err}");
}
