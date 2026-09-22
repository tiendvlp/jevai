//! End-to-end check against the live TypeSafe API, through [`JevClient`].
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example live
//! ```

use std::error::Error as StdError;
use std::time::Duration;

use jevai::client::Error as ClientError;
use jevai::{
    Answer, InvalidCriteria, JevClient, Map, Model, NoulCriteria, Question, Request, Response,
};

const STATE: &str = "Help! My payouts have been failing for 3 days. \
                     I've emailed support twice and nobody has replied.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn StdError>> {
    let key = std::env::var("TYPESAFE_API_KEY")
        .map_err(|_| "set TYPESAFE_API_KEY before running this example")?;

    let client = JevClient::builder(&key)
        .timeout(Duration::from_secs(30))
        .retries(3)
        .build()?;

    let request = Request::new(STATE)
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
                    .with("technical", "Bugs, outages, integrations")
                    .with("sales", "Pricing, upgrades, new accounts"),
            )?,
        )
        .ask(
            "frustration",
            Question::score(
                "How frustrated is the customer?",
                ["Calm", "Frustrated", "Very angry"],
            )?,
        );

    println!("→ POST {}\n  {}\n", jevai::ENDPOINT, serde_json::to_string(&request)?);

    let response = client.send(&request).await?;
    report(&response);
    archive(&response)?;
    demo_errors(&client, &key).await?;

    Ok(())
}

fn report(response: &Response) {
    println!(
        "← answered by {} ({} input / {} output tokens)\n",
        response.model, response.usage.input_tokens, response.usage.output_tokens
    );

    for (id, answer) in response.answers.iter() {
        match answer {
            Answer::Noul(noul) => println!(
                "  {id:<12} noul   {:.2}  (certainty {:.2}, yes={})",
                noul.noul,
                noul.certainty(),
                noul.is_yes(0.5)
            ),
            Answer::Choice(choice) => {
                println!(
                    "  {id:<12} choice {:<10} (confidence {:.2})",
                    choice.choice, choice.confidence
                );
                for (option, p) in choice.probabilities.iter() {
                    println!("  {:<12}   {option:<12} {p:.3}", "");
                }
            }
            Answer::Score(score) => println!(
                "  {id:<12} score  {:.2} → {:?} (confidence {:.2})",
                score.score,
                score.nearest_label(),
                score.confidence
            ),
        }
    }
}

/// The same live value, archived and read back with no parse step.
#[cfg(feature = "rkyv")]
fn archive(response: &Response) -> Result<(), Box<dyn StdError>> {
    use jevai::message::ArchivedResponse;
    use jevai::rkyv::rancor::Error as RkyvError;

    let json_len = serde_json::to_string(response)?.len();
    let bytes = jevai::rkyv::to_bytes::<RkyvError>(response)?;
    let archived = jevai::rkyv::access::<ArchivedResponse, RkyvError>(&bytes)?;

    println!("\n  rkyv: {} bytes archived vs {json_len} bytes JSON", bytes.len());
    println!("  zero-copy read of model field: {}", archived.model.as_str());

    let restored: Response = jevai::rkyv::from_bytes::<Response, RkyvError>(&bytes)?;
    assert_eq!(&restored, response, "archive round trip changed the response");
    println!("  archive round trip matches the parsed response ✓");
    Ok(())
}

#[cfg(not(feature = "rkyv"))]
fn archive(_: &Response) -> Result<(), Box<dyn StdError>> {
    Ok(())
}

/// Typed failures. Note how few of them are even reachable from typed code.
async fn demo_errors(client: &JevClient, key: &str) -> Result<(), Box<dyn StdError>> {
    println!("\n--- typed error handling ---");

    // Reachable: the model name is an open enum, so a bad one compiles.
    let bad_model = Request::new("x")
        .with_model(Model::Version("gpt-4".into()))
        .ask("q", Question::noul("y?"));
    report_error("unknown model", client.send(&bad_model).await.unwrap_err());

    // Reachable: a key is just a string until the server sees it.
    let bad_key = JevClient::new(format!("{key}_wrong"))?;
    report_error(
        "bad api key",
        bad_key
            .send(&Request::new("x").ask("q", Question::noul("y?")))
            .await
            .unwrap_err(),
    );

    // NOT reachable: the server rejects an empty Choice with a 400, but
    // ChoiceCriteria refuses to construct one, so the failure moves off the
    // network and into the type system.
    match Question::choice("w", Map::new()) {
        Err(InvalidCriteria::NoChoiceOptions) => {
            println!("  {:<22} rejected before any request was sent", "empty choice criteria");
        }
        other => panic!("expected a construction failure, got {other:?}"),
    }

    Ok(())
}

fn report_error(label: &str, error: ClientError) {
    match &error {
        ClientError::Api { status, error: api } => {
            println!(
                "  {label:<22} {status} retryable={:<5} {api}",
                status.is_retryable()
            );
            if let Some(kind) = api.error_type() {
                println!("  {:<22} error_type={kind}", "");
            }
            for path in api.invalid_fields() {
                println!("  {:<22} invalid field: {path:?}", "");
            }
        }
        other => println!("  {label:<22} {other}"),
    }
}
