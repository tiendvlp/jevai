# Async client for the TypeSafe (Jev) API

> **Partly superseded 2026-09-23.** The retry decisions below were reversed;
> see [2026-09-23-no-retry-head-body-split.md](2026-09-23-no-retry-head-body-split.md).
> Everything else still holds.

**Date:** 2026-09-22 · **Status:** implemented · **Module:** `src/client.rs`

## Goal

Send `Request`, get `Response`. Async, lightweight, works on `wasm32-unknown-unknown`,
errors usable with `thiserror`/`anyhow`.

## Why there is no separate ECS layer

The ECS shape the brief asked for already exists in the message types:

| ECS | Jev |
|---|---|
| Resource | `Request.state` — one shared value |
| Entity | question id |
| Component | `NoulQuestion` / `ChoiceQuestion` / `ScoreQuestion` |
| Component written back | `Answer`, keyed by the same id |
| System over a query | one HTTP call evaluating all questions in parallel |

`state` is ingested once and every question is evaluated against it concurrently,
and **input tokens are billed**, so N questions in one request cost one `state`
rather than N. Batching is already structural. A second architecture on top would
add indirection and buy nothing, so the client is a plain `send(&Request)`.

## Decisions

| Decision | Reason |
|---|---|
| async only, no blocking wrapper | one code path; `join_all` gives fan-out for free |
| retry on by default | matches the official SDKs; `ApiStatus::is_retryable` already classified 429/529 |
| **read timeouts NOT retried by default** | no idempotency key + tokens billed on arrival ⇒ a retry can pay twice |
| connect/DNS failures always retried | provably never reached the server, so nothing was billed |
| `charset` feature dropped | API is UTF-8 JSON; saves encoding_rs + mime (−6 crates) |
| `http2` kept | API serves HTTP/2; multiplexing helps fan-out (+9 crates) |
| `retry-after`: seconds form only | the HTTP-date form needs `SystemTime::now()`, which **panics on wasm** |
| xorshift jitter, not `rand` | no dependency, and no clock read (same wasm reason) |
| equal jitter, not full jitter | full jitter can collapse to ~0 and hammer a server that just asked for room |

## WASM

Verified by compiling for `wasm32-unknown-unknown`, not assumed.

- Target-specific optional deps: native gets reqwest+tokio, wasm gets reqwest+gloo-timers.
- `ClientBuilder::timeout` does not exist on wasm — `.timeout()` is accepted and
  documented as a no-op so one codebase compiles for both.
- `user_agent` is native-only; browsers forbid setting it from `fetch`.
- `is_connect()` / `is_dns()` are native-only. **`fetch` never reports whether a
  request reached the server**, so a wasm build treats every transport failure as
  possibly-executed and retries strictly less than native.
- `send()` is a plain `async fn` with **no `Send` bound** — the future is `Send` on
  native and `!Send` on wasm.
- `reqwest::Error` is `Send + Sync` on all targets, so `anyhow` works on wasm too.
  Asserted by a compile-time test.

## Weight (measured)

| Config | Crates |
|---|---|
| types only | 14 |
| + rkyv | 31 |
| + client (rustls) | 129 |
| + client (native-tls) | 127 |
| default | 144 |
| **default, wasm32** | **94** |

An async HTTP client in Rust costs ~115 crates; no configuration avoids it. The
lever is the `client` feature, so types-only consumers pay none of it. wasm is
lighter because the browser supplies fetch and TLS.

`rustls` pulls **aws-lc-rs**, which needs a C compiler and cmake. `native-tls` is
the escape hatch for minimal build environments.

## MSRV

**1.85**, raised from 1.81 by reqwest 0.13. Cargo has no per-feature MSRV, so
types-only builds are also nominally pinned there.

## Testing

- 11 wiremock tests: retry-then-succeed, exhaustion, `retry-after` precedence,
  no-retry on 400, all three error shapes, non-JSON body, undecodable 200, anyhow.
- 7 unit tests: key redaction, unsendable key, backoff growth and cap, jitter bounds.
- wasm is compile-checked in CI; wiremock cannot run there.
- `examples/live.rs` hits the real API. ureq dropped.
