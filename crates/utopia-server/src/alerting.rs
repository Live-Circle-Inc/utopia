//! The raising side of alerting (0005). Storage lives in `utopia_store::alerts`; this module
//! only decides **when to raise**.
//!
//! There is no "when to clear" -- one row per incident, never touched again once written.
//! See that module's docs.

use utopia_core::models::Role;
use utopia_store::alerts;

use crate::state::AppState;

/// How many days alerts are retained. **This is exactly where the price of being purely atomic
/// shows up**: one broken source syncing hourly writes 24 rows a day, and without cleanup this
/// table grows into a second log file.
///
/// 30 days is enough to answer "what was that thing last month", and anything older belongs in
/// `audit_events` and the logs.
const RETAIN_DAYS: i32 = 30;

/// On a job failure, take a look at whether the model endpoint is unavailable.
///
/// **Only raise once the job finally gives up** -- attempts exhausted, or this failure marked as
/// not worth retrying ([`hopeless`], #195). Raising on the intermediate retries just sends the
/// same thing three times over: retrying exists precisely so that nobody has to be disturbed,
/// and raising an alert cancels that out. At most one alert per job, so no dedup state at all.
///
/// The criterion is the **type** of the error, not a match on the error text -- any layer in the
/// call chain that adds one context line changes the text. The classification itself is factored
/// out into [`alert_for`].
pub async fn observe_job_failure(
    state: &AppState,
    job: &utopia_store::jobs::Job,
    err: &anyhow::Error,
) {
    // For the ones marked as not worth retrying **this attempt is the last one**, so raise now
    // (#195). Missing this line means failing faster and yet nobody being told -- worse than
    // not having changed anything
    if job.attempts < job.max_attempts && !utopia_core::is_terminal(err) {
        return;
    }
    let Some((kind, severity)) = alert_for(err) else {
        return;
    };
    // System level: which KB's job hit it does not matter -- the endpoint and the quota are
    // shared by the whole deployment
    if let Err(e) = alerts::raise(
        &state.pool,
        alerts::NewAlert {
            kb_id: None,
            severity,
            kind,
            min_role: Role::Admin,
            subject_type: Some("system"),
            subject_id: None,
            detail: serde_json::json!({ "job": job.kind, "error": err.to_string() }),
        },
    )
    .await
    {
        tracing::warn!(error = %e, %kind, "failed to raise alert");
        return;
    }
    state.emit_alert();
}

/// Whether this failure is still worth retrying.
///
/// **Only running out of credit is hopeless.** Rate limiting recovers on its own, an unreachable
/// endpoint may be a service in the middle of a restart, and other errors have even less reason
/// to be decided once and for all -- only the balance will not grow back by itself within seven
/// minutes (#195).
///
/// The criterion shares its source with [`alert_for`] (`utopia_llm::out_of_credit`, the type and
/// not the text), so "the class reported as error" and "the class we stop retrying" are forever
/// the same class and cannot fork.
pub fn hopeless(err: &anyhow::Error) -> bool {
    utopia_llm::out_of_credit(err).is_some()
}

/// Which class of alert a job failure should raise, and whether to raise one at all.
///
/// **It is factored out as a pure function so that it can be tested**: `observe_job_failure`
/// wants an `AppState` and a real database, whereas what has to be defended here is the
/// classification itself. The three classes call for completely different actions -- out of
/// credit needs someone to top up, rate limiting needs lower concurrency or a raised quota, an
/// unreachable endpoint needs the network and the address checked. **Merging them into one
/// points the person at the wrong place, which costs more time than not raising anything.**
///
/// The order is "put what should be said first, first", not a matter of overlapping criteria:
/// the types themselves are mutually exclusive, the overlap is in the status codes (OpenAI
/// returns 429 for an exhausted balance too), and that layer has already been separated out in
/// `utopia_llm::failure`.
fn alert_for(err: &anyhow::Error) -> Option<(&'static str, &'static str)> {
    // Out of credit comes before rate limiting. Their status codes overlap -- OpenAI returns
    // 429 for an exhausted balance too -- and the classification is already finished inside
    // `utopia_llm::failure`, so the order here only puts "the thing that should be said first"
    // first: no money is something a human has to act on, rate limiting is not
    if utopia_llm::out_of_credit(err).is_some() {
        // `error`: it will not get better on its own; extraction stays stopped until someone
        // tops up
        return Some((alerts::kind::LLM_OUT_OF_CREDIT, "error"));
    }
    if utopia_llm::rate_limited(err).is_some() {
        // `warning` and not `error`: the quota recovers on its own, a dead endpoint does not
        return Some((alerts::kind::LLM_RATE_LIMITED, "warning"));
    }
    if utopia_llm::is_unreachable(err) {
        return Some((alerts::kind::LLM_UNREACHABLE, "error"));
    }
    None
}

/// Sweep expired alerts once a day.
pub fn spawn_retention_sweep(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match alerts::purge_older_than(&state.pool, RETAIN_DAYS).await {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, days = RETAIN_DAYS, "purged expired alerts");
                    state.emit_alert();
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "failed to purge expired alerts"),
            }
        }
    });
}

/// The data source got mounted, but its table schema was never ingested.
///
/// **What is reported is not that one failure, it is the state it left behind.** Mounting is two
/// steps: write `kb_data_sources`, then turn the schema into a document and ingest it into the
/// KB. When the first step succeeds and the second does not, the source really is mounted --
/// `query_data` will enqueue, while the model cannot retrieve any table schema and is left
/// guessing at column names.
///
/// The error at the moment of mounting is only visible to the person who clicked the button.
/// From then on this KB just stays silently incomplete, and that is exactly what 0009 is about:
/// what really hurts is not failure, it is failure without a sound.
pub async fn observe_schema_sync_failure(
    state: &AppState,
    kb_id: uuid::Uuid,
    source_id: uuid::Uuid,
    source_name: &str,
    err: &anyhow::Error,
) {
    if let Err(e) = alerts::raise(
        &state.pool,
        alerts::NewAlert {
            kb_id: Some(kb_id),
            severity: "warning",
            kind: alerts::kind::SCHEMA_SYNC_FAILED,
            // Configuration class: what needs fixing is the connection string or the network,
            // not the content. Goes to admin
            min_role: Role::Admin,
            subject_type: Some("data_source"),
            subject_id: Some(source_id),
            // Keep a copy of the name here -- once the source is deleted subject_id no longer
            // resolves to a name, and the alert should outlive it
            detail: serde_json::json!({
                "source": source_name,
                "error": err.to_string(),
            }),
        },
    )
    .await
    {
        tracing::warn!(error = %e, "failed to raise data_source.schema_sync_failed");
        return;
    }
    state.emit_alert();
}

#[cfg(test)]
mod tests {
    use super::alert_for;
    use utopia_llm::{OutOfCredit, RateLimited};
    use utopia_store::alerts::kind;

    /// Rate limiting and an unreachable endpoint are two different things, and the alerts they
    /// raise have to stay separate.
    ///
    /// The consequence of merging them is not "the label looks bad": `llm.unreachable` says the
    /// endpoint cannot be reached, so the admin goes to check the network and the address, while
    /// what actually needs doing is lowering concurrency or raising the quota -- **the alert
    /// pointed the person at the wrong place**, which costs more time than not raising it.
    #[test]
    fn a_rate_limit_is_not_an_unreachable_endpoint() {
        let err = anyhow::Error::new(RateLimited {
            status: 429,
            retry_after: None,
            detail: "TPM limit reached".into(),
        });
        assert_eq!(alert_for(&err), Some((kind::LLM_RATE_LIMITED, "warning")));
    }

    /// **The decision has to see through the context layers.** The extraction path adds at
    /// least one line saying "still failing after 5 rate-limit backoffs", and a decision based
    /// on text matching would be dead the same day.
    #[test]
    fn it_survives_context_layers() {
        let err = anyhow::Error::new(RateLimited {
            status: 429,
            retry_after: None,
            detail: "slow down".into(),
        })
        .context("still failing after 5 rate-limit backoffs")
        .context("extract_document failed");
        assert_eq!(alert_for(&err), Some((kind::LLM_RATE_LIMITED, "warning")));
    }

    /// Out of credit and rate limiting must stay separate: one needs a human to top up, the
    /// other just needs a short wait.
    ///
    /// Reporting the wrong direction has a real, measured cost -- in a real run 14 documents
    /// failed in their entirety because of an empty balance, and at the time the only way to
    /// learn why was to dig `graph_error` out of the database.
    #[test]
    fn out_of_credit_is_not_a_rate_limit() {
        let err = anyhow::Error::new(OutOfCredit {
            status: 402,
            detail: "Sorry, your account balance is insufficient".into(),
        });
        assert_eq!(alert_for(&err), Some((kind::LLM_OUT_OF_CREDIT, "error")));
    }

    /// The decision has to see through the context layers here too.
    #[test]
    fn out_of_credit_survives_context_layers() {
        let err = anyhow::Error::new(OutOfCredit {
            status: 402,
            detail: "insufficient balance".into(),
        })
        .context("bootstrap_ontology failed");
        assert_eq!(alert_for(&err), Some((kind::LLM_OUT_OF_CREDIT, "error")));
    }

    /// The negative case: an ordinary failure should not raise an alert, otherwise every parse
    /// error pops one up and people learn to ignore them.
    #[test]
    fn an_ordinary_failure_raises_nothing() {
        let err = anyhow::anyhow!("failed to parse the result").context("extraction failed");
        assert_eq!(alert_for(&err), None);
    }

    /// The negative case: a clean 4xx is neither rate limiting nor unreachable. A wrong key
    /// needs the key replaced, and that calls for neither lower concurrency nor a network check.
    #[test]
    fn an_auth_failure_raises_nothing() {
        let err = anyhow::anyhow!("LLM request failed (401 Unauthorized): bad key");
        assert_eq!(alert_for(&err), None);
    }

    /// **The balance will not grow back by itself within seven minutes**, so those three
    /// backoff retries only repeat the same error three times over, and push the failure that
    /// should have been seen back by seven minutes (#195).
    #[test]
    fn an_empty_balance_is_not_worth_retrying() {
        let err = anyhow::Error::new(OutOfCredit {
            status: 402,
            detail: "insufficient balance".into(),
        })
        .context("extraction failed");
        assert!(super::hopeless(&err));
    }

    /// Rate limiting, by contrast, is exactly what that backoff is there to serve -- this is
    /// the whole point of keeping the two classes separate (#176).
    #[test]
    fn a_rate_limit_still_gets_its_retries() {
        let err = anyhow::Error::new(RateLimited {
            status: 429,
            retry_after: Some(std::time::Duration::from_secs(30)),
            detail: "TPM limit reached".into(),
        });
        assert!(!super::hopeless(&err));
    }

    /// The negative case: an ordinary failure gets its retries as usual. One network hiccup
    /// should not be decided once and for all.
    #[test]
    fn an_ordinary_failure_still_gets_its_retries() {
        assert!(!super::hopeless(&anyhow::anyhow!(
            "failed to parse the result"
        )));
    }
}
