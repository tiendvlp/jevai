//! Async HTTP client for `POST /v1/systemone`.
//!
//! Compiles for native targets and for `wasm32-unknown-unknown`. On native it
//! runs on hyper/tokio; in a browser it runs on `fetch`, where the browser
//! supplies TLS and scheduling. Exactly one thing differs between the two: the
//! request timeout, which `fetch` does not expose.
//!
//! # Two stages
//!
//! [`JevClient::send`] resolves as soon as the response *head* arrives and
//! hands back a [`Received`], which still owns an unread body. Status and
//! headers are available immediately; the body is read only when you ask for
//! it. This is reqwest's own division, and it is why nothing here retries on
//! your behalf — a client that hands out an unread body cannot know whether it
//! is safe to send the request again.
//!
//! ```no_run
//! # use jevai::{JevClient, Question, Request};
//! # async fn demo() -> Result<(), jevai::client::Error> {
//! let client = JevClient::new(std::env::var("TYPESAFE_API_KEY").unwrap())?;
//! let request = Request::new("Payouts have been failing for 3 days.")
//!     .ask("is_urgent", Question::noul("Does this convey urgency?"));
//!
//! let received = client.send(&request).await?;
//! println!("request {:?}", received.request_id());
//!
//! let response = received.json().await?;
//! # Ok(())
//! # }
//! ```
//!
//! When the head is of no interest, [`JevClient::ask`] collapses both stages.
//!
//! # Retrying
//!
//! There is no retry loop. A `429` or `529` surfaces as [`Error::Api`], and
//! [`ApiStatus::is_retryable`] tells you which statuses are worth sending
//! again. [`Received::retry_after`] reports the server's own requested delay.
//!
//! Retry with care: the API has no idempotency key and bills input tokens on
//! arrival, so re-sending a request that may already have been processed can
//! pay for it twice. A response that never arrived is not proof that the
//! request never did.

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};

use crate::error::{ApiError, ApiStatus};
use crate::message::{Request, Response};

/// Default per-request timeout. Ignored on wasm, which has no timeout knob.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Response header carrying the id to quote when reporting a problem.
pub const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

/// Response header carrying upstream processing time, in milliseconds.
pub const SERVER_TIME_HEADER: &str = "x-envoy-upstream-service-time";

/// Renders an optional request id as a trailing note, or nothing.
struct IdNote<'a>(Option<&'a str>);

impl fmt::Display for IdNote<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(id) => write!(f, " (request {id})"),
            None => Ok(()),
        }
    }
}

/// Anything that can go wrong sending a request or reading its body.
///
/// Derives [`thiserror::Error`] and is `Send + Sync + 'static` on every target,
/// so it drops straight into [`anyhow`](https://docs.rs/anyhow) with `?`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The API rejected the request and explained why.
    #[error("api returned {status}: {error}{}", IdNote(.request_id.as_deref()))]
    Api {
        /// The HTTP status, interpreted.
        status: ApiStatus,
        /// The parsed error body.
        error: ApiError,
        /// Value of [`REQUEST_ID_HEADER`], when the server sent one.
        request_id: Option<String>,
    },

    /// A non-2xx response whose body was not a recognizable API error — a
    /// proxy or gateway page, usually. The body is kept rather than discarded.
    #[error("api returned {status} with an unrecognized body{}: {body}", IdNote(.request_id.as_deref()))]
    UnexpectedBody {
        /// The HTTP status, interpreted.
        status: ApiStatus,
        /// The raw response body, truncated to a readable length.
        body: String,
        /// Value of [`REQUEST_ID_HEADER`], when the server sent one.
        request_id: Option<String>,
    },

    /// A 2xx response that did not parse as a [`Response`].
    #[error("could not decode a successful response{}: {source}", IdNote(.request_id.as_deref()))]
    Decode {
        /// The underlying parse failure.
        #[source]
        source: serde_json::Error,
        /// Value of [`REQUEST_ID_HEADER`], when the server sent one.
        request_id: Option<String>,
    },

    /// The request could not be serialized.
    #[error("could not encode the request: {0}")]
    Encode(#[source] serde_json::Error),

    /// The API key cannot be put in a header (control characters, newline).
    #[error("the api key contains characters that cannot be sent in a header")]
    InvalidApiKey,

    /// Connection, TLS, timeout, or client construction failure.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
}

impl Error {
    /// The server-assigned request id, when the failure carried one.
    ///
    /// Quote this when reporting a problem to the API operator.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. }
            | Self::UnexpectedBody { request_id, .. }
            | Self::Decode { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }

    /// The interpreted status, for failures that reached the server.
    #[must_use]
    pub fn status(&self) -> Option<ApiStatus> {
        match self {
            Self::Api { status, .. } | Self::UnexpectedBody { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// An API key that never prints itself.
#[derive(Clone)]
struct ApiKey(HeaderValue);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"***\"")
    }
}

/// Builds a configured [`JevClient`].
#[derive(Debug)]
pub struct JevClientBuilder {
    api_key: String,
    endpoint: String,
    timeout: Duration,
}

impl JevClientBuilder {
    /// Overrides the evaluation endpoint. Useful for a proxy or a test server.
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Sets the per-request timeout.
    ///
    /// **No effect on `wasm32-unknown-unknown`** — `fetch` exposes no timeout,
    /// so the browser decides. Accepted rather than removed so one codebase
    /// compiles for both targets.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Builds the client.
    ///
    /// # Errors
    /// [`Error::InvalidApiKey`] if the key cannot go in a header, or
    /// [`Error::Transport`] if the underlying HTTP client cannot be built.
    pub fn build(self) -> Result<JevClient, Error> {
        let mut auth = HeaderValue::try_from(format!("Bearer {}", self.api_key))
            .map_err(|_| Error::InvalidApiKey)?;
        // Marks the header so middleware and debug output redact it.
        auth.set_sensitive(true);

        let builder = reqwest::Client::builder();

        // Native only: wasm's ClientBuilder has no timeout or user-agent. The
        // browser forbids setting User-Agent from fetch.
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder
            .timeout(self.timeout)
            .user_agent(concat!("jevai/", env!("CARGO_PKG_VERSION")));

        Ok(JevClient {
            http: builder.build()?,
            auth: ApiKey(auth),
            endpoint: self.endpoint,
        })
    }
}

/// An async client for the TypeSafe evaluation endpoint.
///
/// Cloning is cheap — the inner HTTP client shares one connection pool — so
/// build one and share it. Concurrent requests reuse the same connection over
/// HTTP/2.
#[derive(Clone, Debug)]
pub struct JevClient {
    http: reqwest::Client,
    auth: ApiKey,
    endpoint: String,
}

impl JevClient {
    /// Builds a client with the default timeout.
    ///
    /// # Errors
    /// See [`JevClientBuilder::build`].
    pub fn new(api_key: impl Into<String>) -> Result<Self, Error> {
        Self::builder(api_key).build()
    }

    /// Starts configuring a client.
    #[must_use]
    pub fn builder(api_key: impl Into<String>) -> JevClientBuilder {
        JevClientBuilder {
            api_key: api_key.into(),
            endpoint: crate::ENDPOINT.to_owned(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// The endpoint this client posts to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Sends one request and returns once the response head has arrived.
    ///
    /// The body is left unread on the wire. A non-2xx status is *not* an error
    /// here — the status is known but the explanation is still in the body, so
    /// classification happens in [`Received::json`].
    ///
    /// The returned future is `Send` on native targets and `!Send` on wasm,
    /// because a `fetch` future is not `Send`. No `Send` bound is imposed
    /// anywhere, so the same call compiles for both.
    ///
    /// # Errors
    /// [`Error::Encode`] if the request will not serialize, or
    /// [`Error::Transport`] if the exchange never produced a response head.
    pub async fn send(&self, request: &Request) -> Result<Received, Error> {
        let body = serde_json::to_vec(request).map_err(Error::Encode)?;

        let http = self
            .http
            .post(&self.endpoint)
            .header(AUTHORIZATION, self.auth.0.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;

        Ok(Received { http })
    }

    /// Sends one request and reads the answer, discarding the head.
    ///
    /// Shorthand for `send(request).await?.json().await`. Use [`send`] instead
    /// when you want the request id or the response headers.
    ///
    /// [`send`]: Self::send
    ///
    /// # Errors
    /// Any [`Error`] from either stage.
    pub async fn ask(&self, request: &Request) -> Result<Response, Error> {
        self.send(request).await?.json().await
    }
}

/// A response head, with its body still unread.
///
/// Every accessor here takes `&self` and reads only the head. The body is
/// consumed by [`json`](Self::json), [`bytes`](Self::bytes) or
/// [`text`](Self::text), each of which takes `self` — so the body can be read
/// exactly once, and the compiler enforces it.
#[derive(Debug)]
pub struct Received {
    http: reqwest::Response,
}

impl Received {
    /// The HTTP status, interpreted.
    #[must_use]
    pub fn status(&self) -> ApiStatus {
        ApiStatus::from_code(self.http.status().as_u16())
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.http.status().is_success()
    }

    /// Every response header.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        self.http.headers()
    }

    /// The server-assigned request id, from [`REQUEST_ID_HEADER`].
    ///
    /// Quote this when reporting a problem to the API operator.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.http.headers().get(REQUEST_ID_HEADER)?.to_str().ok()
    }

    /// How long the upstream spent on this request, from
    /// [`SERVER_TIME_HEADER`].
    ///
    /// Compare against your own measured round trip to separate evaluation
    /// time from network time.
    #[must_use]
    pub fn server_time(&self) -> Option<Duration> {
        let millis: u64 = self
            .http
            .headers()
            .get(SERVER_TIME_HEADER)?
            .to_str()
            .ok()?
            .trim()
            .parse()
            .ok()?;
        Some(Duration::from_millis(millis))
    }

    /// The delay the server asked for before trying again, if any.
    ///
    /// Only the delta-seconds form is read. The HTTP-date form would need
    /// wall-clock "now", and `SystemTime::now()` panics on
    /// `wasm32-unknown-unknown`, so a date-valued header reports `None` rather
    /// than breaking the browser build.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        parse_retry_after(self.http.headers().get(RETRY_AFTER))
    }

    /// The body length the server declared, if it declared one.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.http.content_length()
    }

    /// Reads the body and interprets it.
    ///
    /// # Errors
    /// [`Error::Api`] or [`Error::UnexpectedBody`] for a non-2xx status,
    /// [`Error::Decode`] for a 2xx body that is not a [`Response`], and
    /// [`Error::Transport`] if the body could not be read.
    pub async fn json(self) -> Result<Response, Error> {
        let status = self.status();
        let success = self.is_success();
        let request_id = self.request_id().map(ToOwned::to_owned);

        let bytes = self.http.bytes().await?;

        if success {
            return serde_json::from_slice(&bytes).map_err(|source| Error::Decode {
                source,
                request_id,
            });
        }

        Err(match serde_json::from_slice::<ApiError>(&bytes) {
            Ok(error) => Error::Api {
                status,
                error,
                request_id,
            },
            Err(_) => Error::UnexpectedBody {
                status,
                body: truncate(&String::from_utf8_lossy(&bytes)),
                request_id,
            },
        })
    }

    /// Reads the body as raw bytes, whatever the status.
    ///
    /// # Errors
    /// [`Error::Transport`] if the body could not be read.
    pub async fn bytes(self) -> Result<Vec<u8>, Error> {
        Ok(self.http.bytes().await?.to_vec())
    }

    /// Reads the body as text, whatever the status.
    ///
    /// # Errors
    /// [`Error::Transport`] if the body could not be read.
    pub async fn text(self) -> Result<String, Error> {
        Ok(self.http.text().await?)
    }

    /// Unwraps to the underlying reqwest response.
    ///
    /// For the cases this wrapper does not cover — streaming the body with
    /// `bytes_stream`, reading cookies, inspecting the negotiated version.
    #[must_use]
    pub fn into_inner(self) -> reqwest::Response {
        self.http
    }
}

/// Reads a `retry-after` header. Delta-seconds form only; see
/// [`Received::retry_after`].
fn parse_retry_after(header: Option<&HeaderValue>) -> Option<Duration> {
    let seconds: u64 = header?.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds))
}

/// Keeps an unexpected body readable in a log line.
fn truncate(body: &str) -> String {
    const LIMIT: usize = 512;
    if body.len() <= LIMIT {
        return body.to_owned();
    }
    let mut end = LIMIT;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &body[..end], body.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `anyhow::Error` requires `Send + Sync + 'static`. Asserted at compile
    /// time on whichever target is being built.
    #[test]
    fn error_is_anyhow_compatible() {
        fn assert_bounds<T: std::error::Error + Send + Sync + 'static>() {}
        assert_bounds::<Error>();
    }

    #[test]
    fn api_key_never_prints() {
        let client = JevClient::new("apikey_super_secret_value").unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("super_secret"), "{rendered}");
        assert!(rendered.contains("***"), "{rendered}");
    }

    #[test]
    fn rejects_an_unsendable_api_key() {
        assert!(matches!(
            JevClient::new("bad\nkey"),
            Err(Error::InvalidApiKey)
        ));
    }

    #[test]
    fn retry_after_reads_only_the_seconds_form() {
        let secs = HeaderValue::from_static("12");
        assert_eq!(parse_retry_after(Some(&secs)), Some(Duration::from_secs(12)));

        let date = HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT");
        assert_eq!(parse_retry_after(Some(&date)), None, "date form falls back");
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn error_display_mentions_the_request_id() {
        let error = Error::UnexpectedBody {
            status: ApiStatus::from_code(502),
            body: "<html>bad gateway</html>".to_owned(),
            request_id: Some("req_abc123".to_owned()),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("req_abc123"), "{rendered}");
        assert_eq!(error.request_id(), Some("req_abc123"));
    }

    #[test]
    fn error_display_omits_an_absent_request_id() {
        let error = Error::UnexpectedBody {
            status: ApiStatus::from_code(502),
            body: "nope".to_owned(),
            request_id: None,
        };
        assert!(!error.to_string().contains("request "), "{error}");
        assert_eq!(error.request_id(), None);
    }

    #[test]
    fn transport_errors_carry_no_request_id_or_status() {
        let client = JevClient::new("k").unwrap();
        assert_eq!(client.endpoint(), crate::ENDPOINT);
        let error = Error::Encode(serde_json::from_str::<Response>("!").unwrap_err());
        assert_eq!(error.request_id(), None);
        assert_eq!(error.status(), None);
    }

    #[test]
    fn truncate_keeps_short_bodies_and_cuts_long_ones() {
        assert_eq!(truncate("short"), "short");
        let long = "x".repeat(1000);
        let cut = truncate(&long);
        assert!(cut.contains("1000 bytes total"));
        assert!(cut.len() < 600);
    }
}
