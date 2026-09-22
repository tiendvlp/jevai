# Drop retry, split head from body

*2026-09-23. Supersedes the retry decisions in
[2026-09-22-reqwest-client.md](2026-09-22-reqwest-client.md).*

## Question

Can the client split response headers from the response body the way reqwest
does?

## What reqwest actually does

`send()` resolves as soon as the response **head** arrives. hyper has parsed
status and headers; the body is still an unread `Incoming`. The split is
enforced by receiver type, not by convention:

| | Receiver | Reads the body |
|---|---|---|
| `status()`, `headers()`, `content_length()` | `&self` | no |
| `text()`, `bytes()`, `json()` | `self` | yes, all of it |
| `chunk()` | `&mut self` | incrementally |
| `bytes_stream()` | `self`, needs `stream` | incrementally |

Two findings from reading the 0.13.5 source:

- `json()` is **not** incremental. It calls `do_bytes()` and then
  `serde_json::from_slice` on the whole buffer — the same thing this client
  was already doing.
- `chunk()` does not exist on wasm. `wasm/response.rs::bytes()` calls
  `.array_buffer()`, which resolves the entire body. So no incremental read is
  portable across our two targets.

## Why the body half is not worth building

The API does not stream. Every "stream" hit in the published docs is
incidental — "downstream model", a HuggingFace `streaming=True`, and one line
that says outright that Jev does not stream text. Observed responses are a
single small JSON object, `content-length: 114` on a three-question request.
`bytes_stream()` would add the `stream` feature to wait for the same bytes.

## Why the head half was worth building

The live API sends headers this client was discarding:

```
x-typesafe-request-id: req_01a0c98b136c705483fefe22044d82bc
x-envoy-upstream-service-time: 101
```

The request id is what you quote when reporting a problem. The old `attempt()`
read `headers()` for `retry-after` and threw the rest away.

## The blocker was retry, and retry is now gone

Retry needs the body — that is where the typed error lives — and it needs the
right to send the request again. A client that hands out an unread body can do
neither. reqwest does not retry *because* it splits; this client split nothing
*because* it retried.

Removing retry resolves the conflict in the direction the API justifies
anyway: no idempotency key, input tokens billed on arrival, so the caller is
better placed than the library to decide whether re-sending is safe.

## Decisions

| Decision | Reason |
|---|---|
| no retry loop at all | the caller owns the billing risk; the library cannot judge it |
| `send()` returns `Received`, body unread | reqwest's own division, enforced by `&self` vs `self` |
| non-2xx is not an error from `send()` | the head knows the status, not the explanation |
| `json()` classifies | needs the body to build `Error::Api` |
| `ask()` as a one-call shorthand | `send().await?.json().await` for callers who want neither head nor control |
| `request_id` carried into `Error` | `json()` consumes the head; the id matters most on failure |
| `retry_after()` kept as an accessor | 429 still says how long to wait — now advice, not an action |
| `into_inner()` escape hatch | `bytes_stream`, cookies, version: reachable without us modelling them |
| `ApiStatus::is_retryable` kept | it always was classification, not behaviour |

## What this removed

`RetryPolicy`, the backoff loop, equal jitter, the xorshift PRNG, the
`AtomicU64` seed, the hand-written `Clone` it forced, `Error::RetriesExhausted`,
`transport_is_retryable`, and `sleep`.

Four of the five non-test `cfg(target_arch)` blocks went with them —
`transport_is_retryable` needed them because wasm `fetch` has no
`is_connect`/`is_dns`, and `sleep` needed them for tokio vs gloo. One cfg
remains: the native-only builder timeout.

**Dependency effect, measured with `cargo tree -e normal`:**

| Target | Before | After |
|---|---|---|
| native (`client,rustls-tls`) | 98 crates | **98 crates** |
| wasm (`client`) | 27 crates | **25 crates** |

Native is unchanged: `tokio` was a direct dependency only for `sleep`, but
hyper pulls it in regardless, so dropping the direct edge changed nothing in
the graph. Only wasm actually shrinks, losing `gloo-timers` and
`futures-channel`.

## Testing

- 16 wiremock tests: head metadata before the body, absent headers as `None`,
  non-2xx deferred to `json()`, exactly-once delivery of a 529, `retry-after`
  seconds and HTTP-date, all three error shapes, request id into the error,
  non-JSON body, undecodable 200, raw `bytes`/`text`, `into_inner`, anyhow.
- 8 unit tests: key redaction, unsendable key, `retry-after` parsing, request
  id in `Display` both present and absent, truncation.
- wasm is compile-checked; wiremock cannot run there.
