//! Async HTTP client for `POST /v1/systemone`.
//!
//! Compiles for native targets and for `wasm32-unknown-unknown`. On native it
//! runs on hyper/tokio; in a browser it runs on `fetch`, where the browser
//! supplies TLS and scheduling. The differences are confined to two places,
//! both marked below: the request timeout and the backoff sleep.
//!
//! ```no_run
//! # use jevai::{JevClient, Question, Request};
//! # async fn demo() -> Result<(), jevai::client::Error> {
//! let client = JevClient::new(std::env::var("TYPESAFE_API_KEY").unwrap())?;
//! let request = Request::new("Payouts have been failing for 3 days.")
//!     .ask("is_urgent", Question::noul("Does this convey urgency?"));
//!
//! let response = client.send(&request).await?;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};

use crate::error::{ApiError, ApiStatus};
use crate::message::{Request, Response};

/// Default per-request timeout. Ignored on wasm, which has no timeout knob.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Anything that can go wrong sending a request.
///
/// Derives [`thiserror::Error`] and is `Send + Sync + 'static` on every target,
/// so it drops straight into [`anyhow`](https://docs.rs/anyhow) with `?`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The API rejected the request and explained why.
    #[error("api returned {status}: {error}")]
    Api {
        /// The HTTP status, interpreted.
        status: ApiStatus,
        /// The parsed error body.
        error: ApiError,
    },

    /// A non-2xx response whose body was not a recognizable API error — a
    /// proxy or gateway page, usually. The body is kept rather than discarded.
    #[error("api returned {status} with an unrecognized body: {body}")]
    UnexpectedBody {
        /// The HTTP status, interpreted.
        status: ApiStatus,
        /// The raw response body, truncated to a readable length.
        body: String,
    },

    /// A 2xx response that did not parse as a [`Response`].
    #[error("could not decode a successful response: {0}")]
    Decode(#[source] serde_json::Error),

    /// The request could not be serialized.
    #[error("could not encode the request: {0}")]
    Encode(#[source] serde_json::Error),

    /// The API key cannot be put in a header (control characters, newline).
    #[error("the api key contains characters that cannot be sent in a header")]
    InvalidApiKey,

    /// Connection, TLS, timeout, or client construction failure.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// Every retry was used up. Carries the final failure.
    #[error("gave up after {attempts} attempts: {last}")]
    RetriesExhausted {
        /// How many attempts were made in total.
        attempts: u32,
        /// The failure from the last attempt.
        #[source]
        last: Box<Error>,
    },
}

/// How failed requests are retried.
///
/// The defaults retry only what is safe to retry. See
/// [`retry_read_timeouts`](Self::retry_read_timeouts) for the one case that is
/// genuinely a judgement call.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Total attempts including the first. `1` disables retrying.
    pub max_attempts: u32,
    /// Delay before the second attempt; doubles from there by `multiplier`.
    pub initial_backoff: Duration,
    /// Ceiling for a single backoff.
    pub max_backoff: Duration,
    /// Growth factor applied per attempt.
    pub multiplier: f64,
    /// Spread retries out so concurrent clients do not resynchronize.
    pub jitter: bool,
    /// Retry a request that timed out while waiting for the response body.
    ///
    /// Off by default, and this is deliberate. The API has no idempotency key,
    /// and input tokens are billed on arrival, so a request that timed out may
    /// already have been processed and charged. Retrying it can pay twice.
    /// Connection failures are always retried — those never reached the server.
    pub retry_read_timeouts: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: true,
            retry_read_timeouts: false,
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
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
    retry: RetryPolicy,
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

    /// Replaces the whole retry policy.
    #[must_use]
    pub fn retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Sets the total number of attempts, including the first.
    #[must_use]
    pub fn retries(mut self, max_attempts: u32) -> Self {
        self.retry.max_attempts = max_attempts.max(1);
        self
    }

    /// Sets the delay before the second attempt.
    #[must_use]
    pub fn initial_backoff(mut self, backoff: Duration) -> Self {
        self.retry.initial_backoff = backoff;
        self
    }

    /// Enables retrying requests that timed out waiting for a response.
    ///
    /// Read [`RetryPolicy::retry_read_timeouts`] before turning this on: it can
    /// double-bill a request the server already processed.
    #[must_use]
    pub fn retry_read_timeouts(mut self, retry: bool) -> Self {
        self.retry.retry_read_timeouts = retry;
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
            retry: self.retry,
            rng: AtomicU64::new(0x2545_F491_4F6C_DD1D),
        })
    }
}

/// An async client for the TypeSafe evaluation endpoint.
///
/// Cloning is cheap — the inner HTTP client shares one connection pool — so
/// build one and share it. Concurrent requests reuse the same connection over
/// HTTP/2.
#[derive(Debug)]
pub struct JevClient {
    http: reqwest::Client,
    auth: ApiKey,
    endpoint: String,
    retry: RetryPolicy,
    rng: AtomicU64,
}

impl Clone for JevClient {
    /// Clones share the connection pool; only the jitter state is forked so
    /// two clones do not emit an identical backoff sequence.
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            auth: self.auth.clone(),
            endpoint: self.endpoint.clone(),
            retry: self.retry.clone(),
            rng: AtomicU64::new(self.next_rand()),
        }
    }
}

impl JevClient {
    /// Builds a client with default timeout and retry policy.
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
            retry: RetryPolicy::default(),
        }
    }

    /// The retry policy in force.
    #[must_use]
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }

    /// Sends one request, retrying per the policy.
    ///
    /// The returned future is `Send` on native targets and `!Send` on wasm,
    /// because a `fetch` future is not `Send`. No `Send` bound is imposed
    /// anywhere, so the same call compiles for both.
    ///
    /// # Errors
    /// [`Error::Api`] when the server rejected the request, [`Error::Transport`]
    /// for network failures, [`Error::RetriesExhausted`] when retries ran out.
    pub async fn send(&self, request: &Request) -> Result<Response, Error> {
        let body = serde_json::to_vec(request).map_err(Error::Encode)?;
        let mut attempt = 0;

        loop {
            attempt += 1;
            let retryable = match self.attempt(&body).await {
                Outcome::Done(response) => return Ok(response),
                Outcome::Fatal(error) => return Err(error),
                Outcome::Retry { after, error } => (after, error),
            };
            let (retry_after, error) = retryable;

            if attempt >= self.retry.max_attempts {
                return Err(Error::RetriesExhausted {
                    attempts: attempt,
                    last: Box::new(error),
                });
            }

            sleep(retry_after.unwrap_or_else(|| self.backoff(attempt))).await;
        }
    }

    /// One trip to the server, classified.
    async fn attempt(&self, body: &[u8]) -> Outcome {
        let sent = self
            .http
            .post(&self.endpoint)
            .header(AUTHORIZATION, self.auth.0.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .await;

        let http = match sent {
            Ok(http) => http,
            Err(error) => {
                return if self.transport_is_retryable(&error) {
                    Outcome::Retry {
                        after: None,
                        error: Error::Transport(error),
                    }
                } else {
                    Outcome::Fatal(Error::Transport(error))
                };
            }
        };

        let status = ApiStatus::from_code(http.status().as_u16());
        let retry_after = parse_retry_after(http.headers().get(RETRY_AFTER));
        let success = http.status().is_success();

        let bytes = match http.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return Outcome::Fatal(Error::Transport(error)),
        };

        if success {
            return match serde_json::from_slice::<Response>(&bytes) {
                Ok(response) => Outcome::Done(response),
                Err(error) => Outcome::Fatal(Error::Decode(error)),
            };
        }

        let error = match serde_json::from_slice::<ApiError>(&bytes) {
            Ok(error) => Error::Api { status, error },
            Err(_) => Error::UnexpectedBody {
                status,
                body: truncate(&String::from_utf8_lossy(&bytes)),
            },
        };

        if status.is_retryable() {
            Outcome::Retry {
                after: retry_after,
                error,
            }
        } else {
            Outcome::Fatal(error)
        }
    }

    /// Whether a transport failure can be retried without risking a double bill.
    ///
    /// On native targets a connect or DNS failure provably never reached the
    /// server, so nothing was billed and retrying is free.
    ///
    /// In a browser there is no such signal: `fetch` reports a failure without
    /// saying whether the request was delivered, so every transport failure is
    /// treated as possibly-executed and follows the opt-in timeout policy. A
    /// wasm build therefore retries strictly less than a native one.
    fn transport_is_retryable(&self, error: &reqwest::Error) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        let never_reached_server = error.is_connect() || error.is_dns();
        #[cfg(target_arch = "wasm32")]
        let never_reached_server = false;

        if never_reached_server {
            return true;
        }

        self.retry.retry_read_timeouts && (error.is_timeout() || error.is_request())
    }

    /// Exponential backoff with equal jitter.
    fn backoff(&self, attempt: u32) -> Duration {
        let base = self.retry.initial_backoff.as_millis() as f64
            * self.retry.multiplier.powi(attempt.saturating_sub(1) as i32);
        let capped = base.min(self.retry.max_backoff.as_millis() as f64).max(0.0);

        let millis = if self.retry.jitter {
            // Equal jitter: half fixed, half random. Full jitter can collapse
            // to ~0 and hammer a server that just asked for room.
            let unit = (self.next_rand() >> 11) as f64 / (1u64 << 53) as f64;
            capped / 2.0 + capped / 2.0 * unit
        } else {
            capped
        };

        Duration::from_millis(millis as u64)
    }

    /// xorshift64*, seeded per client.
    ///
    /// Deliberately not the `rand` crate, and deliberately not clock-seeded:
    /// `SystemTime::now()` panics on `wasm32-unknown-unknown`. Jitter needs
    /// decorrelation, not cryptographic quality.
    fn next_rand(&self) -> u64 {
        let mut x = self
            .rng
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            | 1;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// What one attempt produced.
enum Outcome {
    Done(Response),
    Retry {
        after: Option<Duration>,
        error: Error,
    },
    Fatal(Error),
}

/// Reads a `retry-after` header.
///
/// Only the delta-seconds form is supported. The HTTP-date form would need
/// wall-clock "now", and `SystemTime::now()` panics on
/// `wasm32-unknown-unknown`; a date-valued header falls back to exponential
/// backoff instead of breaking the browser build.
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

/// Backoff sleep. The only other place the targets diverge.
async fn sleep(duration: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(duration).await;

    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::TimeoutFuture::new(duration.as_millis() as u32).await;
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
    fn backoff_grows_and_stays_capped() {
        let client = JevClient::builder("k")
            .retry_policy(RetryPolicy {
                jitter: false,
                initial_backoff: Duration::from_millis(100),
                max_backoff: Duration::from_millis(400),
                ..RetryPolicy::default()
            })
            .build()
            .unwrap();

        assert_eq!(client.backoff(1), Duration::from_millis(100));
        assert_eq!(client.backoff(2), Duration::from_millis(200));
        assert_eq!(client.backoff(3), Duration::from_millis(400));
        assert_eq!(client.backoff(9), Duration::from_millis(400), "capped");
    }

    #[test]
    fn jitter_stays_within_half_the_window_and_varies() {
        let client = JevClient::builder("k")
            .retry_policy(RetryPolicy {
                initial_backoff: Duration::from_millis(1000),
                max_backoff: Duration::from_secs(60),
                ..RetryPolicy::default()
            })
            .build()
            .unwrap();

        let samples: Vec<_> = (0..32).map(|_| client.backoff(1)).collect();
        for sample in &samples {
            assert!(
                *sample >= Duration::from_millis(500) && *sample <= Duration::from_millis(1000),
                "equal jitter must stay in [half, full]: {sample:?}"
            );
        }
        assert!(
            samples.iter().collect::<std::collections::HashSet<_>>().len() > 1,
            "jitter must actually vary"
        );
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
