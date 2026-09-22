# jevai

Typed messages and an async client for the [TypeSafe](https://typesafe.ai)
**System One (Jev)** API, in Rust.

> Unofficial. TypeSafe publishes official SDKs for
> [Python](https://github.com/typesafe-ai/typesafe-sdk-python) and
> [TypeScript](https://github.com/typesafe-ai/typesafe-sdk-js); this is a
> community Rust crate built against their
> [public API documentation](https://docs.typesafe.ai).

## What it does

One `state` plus any number of typed questions, evaluated in parallel by the
API. Three question types — `noul` (a probability), `choice` (a distribution
over named options), and `score` (a point on a labelled scale).

```rust
use jevai::{JevClient, Map, NoulCriteria, Question, Request};

let client = JevClient::new(std::env::var("TYPESAFE_API_KEY")?)?;

let request = Request::new("Help! My payouts have been failing for 3 days.")
    .ask(
        "is_urgent",
        Question::noul("Does this convey urgency?")
            .with_noul_criteria(NoulCriteria::new(
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
        )?,
    );

let response = client.ask(&request).await?;
```

## Design

**Invalid requests mostly do not compile.** Criteria validate at construction,
so an empty choice set or an 11-level score scale is a `Result` at the call
site rather than a `400` from the server.

**Two serializers, one type.** Every message type derives both
[serde](https://serde.rs) and [rkyv](https://rkyv.org). serde attributes
describe the JSON wire form; rkyv derives a separate zero-copy archived layout
from the Rust shape and ignores them. Use serde to talk to the API and rkyv to
cache, memory-map, or IPC the same values with no parse step.

**The client splits head from body,** the way reqwest does:

```rust
let received = client.send(&request).await?;   // head only, body unread

received.status();        // ApiStatus
received.request_id();    // x-typesafe-request-id, for support tickets
received.server_time();   // x-envoy-upstream-service-time
received.retry_after();   // what the server asked for on a 429

let response = received.json().await?;         // body consumed and classified
```

`ask()` collapses both stages when the head does not matter.

**Nothing is retried for you.** The API has no idempotency key and bills input
tokens on arrival, so re-sending a request that may already have been processed
can pay for it twice — a decision the caller is better placed to make.
`ApiStatus::is_retryable()` and `Received::retry_after()` give you what you need
to build your own loop.

## Install

```toml
[dependencies]
jevai = "0.1"
```

Rust 1.85+ (reqwest 0.13's MSRV). Types-only builds work on 1.81.

### Features

| Feature | Default | Effect |
|---|---|---|
| `client` | ✅ | The async reqwest client. Off => types only, no HTTP stack. |
| `rkyv` | ✅ | Zero-copy archival of every message type. |
| `rustls-tls` | ✅ | rustls backend (native targets). |
| `native-tls` | — | OS TLS instead. |

```toml
# Just the types, no HTTP.
jevai = { version = "0.1", default-features = false }
```

### WASM

Builds for `wasm32-unknown-unknown`, where reqwest runs on `fetch` and the
browser supplies TLS. Two caveats: the builder's `timeout` is ignored (`fetch`
exposes no timeout), and the returned futures are `!Send`.

## Examples

```sh
TYPESAFE_API_KEY=... cargo run --example live
```

Sends a three-question request to the live API, prints the answers with the
request id and server timing, then archives the response with rkyv and reads it
back.

## Links

- [TypeSafe](https://typesafe.ai) · [API documentation](https://docs.typesafe.ai)
- Official SDKs: [Python](https://github.com/typesafe-ai/typesafe-sdk-python) ·
  [TypeScript](https://github.com/typesafe-ai/typesafe-sdk-js)
- [Agent skills for System One](https://github.com/typesafe-ai/skills)
- Design notes for this crate: [`docs/design/`](docs/design/)

## License

MIT OR Apache-2.0, at your option.
