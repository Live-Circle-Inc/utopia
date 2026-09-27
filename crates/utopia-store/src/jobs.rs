//! Job queue: consumed with Postgres `FOR UPDATE SKIP LOCKED`.
//! The worker runs in the same process as the API (a tokio task), and failures are retried
//! on a 30s * attempts² backoff -- unless the handler marked it `utopia_core::Terminal`,
//! in which case one attempt is the end of it (see [`retry_delay`]).
//! Concurrent consumption: the scheduling loop keeps dispatching while "running < target",
//! and jobs execute in their own task; the target is read hot through an AtomicUsize --
//! changing the concurrency in the system settings takes effect at once, no restart needed.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use utopia_core::AppResult;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

pub async fn enqueue(pool: &PgPool, kind: &str, payload: serde_json::Value) -> AppResult<i64> {
    let (id,): (i64,) =
        sqlx::query_as("INSERT INTO jobs (kind, payload) VALUES ($1, $2) RETURNING id")
            .bind(kind)
            .bind(payload)
            .fetch_one(pool)
            .await?;
    Ok(id)
}

/// The scope of a failed-job requeue (#216). All three conditions may be empty;
/// empty = unrestricted.
///
/// **Scoping by KB means resolving the payload**: the jobs table has no kb column, and a
/// payload carries only one of `document_id` / `source_id` / `kb_id`, each of which
/// resolves to a KB. System jobs with no KB are only touched when the scope is unrestricted
#[derive(Debug, Default, Clone, Copy)]
pub struct RequeueScope<'a> {
    pub kb_id: Option<Uuid>,
    pub kind: Option<&'a str>,
    /// Only requeue what failed after this instant -- the "run it again" on an alert
    /// circles exactly that outage window
    pub failed_since: Option<DateTime<Utc>>,
}

/// The KB-scope SQL predicate, where `$N` is the KB id; shared by `requeue_failed`
/// and `failed_count`
const KB_SCOPE: &str = "(
       (j.payload ? 'kb_id' AND j.payload->>'kb_id' = $KB::text)
    OR (j.payload ? 'document_id' AND EXISTS (
            SELECT 1 FROM documents d
             WHERE d.id::text = j.payload->>'document_id' AND d.kb_id = $KB))
    OR (j.payload ? 'source_id' AND EXISTS (
            SELECT 1 FROM sources s
             WHERE s.id::text = j.payload->>'source_id' AND s.kb_id = $KB)))";

/// Put the failed jobs in scope back on the queue: `attempts` reset to zero, due now.
///
/// The handlers are all idempotent (reclaiming orphans at startup rests on exactly that),
/// so a requeue is always safe; before this `failed` was the end of the line -- the
/// balance ran out, a whole batch of documents failed, and after topping up the only
/// options were clicking them one at a time or re-extracting the entire source
pub async fn requeue_failed(pool: &PgPool, scope: RequeueScope<'_>) -> AppResult<u64> {
    let sql = format!(
        "UPDATE jobs j
            SET status = 'queued', attempts = 0, run_at = now(), updated_at = now()
          WHERE j.status = 'failed'
            AND ($1::text IS NULL OR j.kind = $1)
            AND ($2::timestamptz IS NULL OR j.updated_at >= $2)
            AND ($3::uuid IS NULL OR {})",
        KB_SCOPE.replace("$KB", "$3")
    );
    let res = sqlx::query(&sql)
        .bind(scope.kind)
        .bind(scope.failed_since)
        .bind(scope.kb_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// How many failed jobs are in scope -- the "N failed jobs" line on the settings page
pub async fn failed_count(pool: &PgPool, kb_id: Option<Uuid>) -> AppResult<i64> {
    let sql = format!(
        "SELECT count(*) FROM jobs j
          WHERE j.status = 'failed' AND ($1::uuid IS NULL OR {})",
        KB_SCOPE.replace("$KB", "$1")
    );
    Ok(sqlx::query_scalar(&sql).bind(kb_id).fetch_one(pool).await?)
}

/// Claim one due job; returns None if there is none.
async fn claim_one(pool: &PgPool) -> AppResult<Option<Job>> {
    let job = sqlx::query_as(
        "UPDATE jobs SET status = 'running', locked_at = now(),
                attempts = attempts + 1, updated_at = now()
         WHERE id = (
             SELECT id FROM jobs
             WHERE status = 'queued' AND run_at <= now()
             ORDER BY run_at
             FOR UPDATE SKIP LOCKED
             LIMIT 1
         )
         RETURNING id, kind, payload, attempts, max_attempts",
    )
    .fetch_optional(pool)
    .await?;
    Ok(job)
}

async fn mark_done(pool: &PgPool, id: i64) -> AppResult<()> {
    // **Success has to clear the previous error.** After a successful retry last_error
    // still held the text from the attempt that failed, so the jobs table showed
    // status='done' paired with an error message -- and whoever was debugging read a
    // cause that no longer held. It misled us exactly once in practice: bootstrap had
    // plainly succeeded, and the table still carried "column relation_type does not exist".
    sqlx::query(
        "UPDATE jobs SET status = 'done', last_error = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// How long to wait before the next retry; `None` = this is the end of it.
///
/// Two reasons to stop here: **the attempts are used up**, or **the handler said this
/// one will not get any better by being retried** (`utopia_core::Terminal`, see issue
/// #195). The latter did not exist before, so a job that had run out of balance walked
/// through all three backoffs anyway -- the balance does not grow back on its own inside
/// seven minutes, so those three attempts only said the same error three times over, and
/// on top of that pushed the "failed" that ops should have seen seven minutes later.
///
/// Rate limiting is the opposite: it is exactly what this backoff exists to serve,
/// because quota does recover on its own. #176 split the two kinds apart precisely so
/// each could take its own path, and the retry policy did not keep up at the time.
///
/// Pulled out as a pure function so it can be tested -- the decision is made here, and
/// the database write only records the decision.
fn retry_delay(attempts: i32, max_attempts: i32, terminal: bool) -> Option<i64> {
    if terminal || attempts >= max_attempts {
        return None;
    }
    Some(30i64 * i64::from(attempts) * i64::from(attempts))
}

async fn mark_failed(pool: &PgPool, job: &Job, err: &anyhow::Error) -> AppResult<()> {
    let text = format!("{err:#}");
    let Some(backoff_secs) = retry_delay(
        job.attempts,
        job.max_attempts,
        utopia_core::is_terminal(err),
    ) else {
        sqlx::query(
            "UPDATE jobs SET status = 'failed', last_error = $2, updated_at = now() WHERE id = $1",
        )
        .bind(job.id)
        .bind(&text)
        .execute(pool)
        .await?;
        return Ok(());
    };
    sqlx::query(
        "UPDATE jobs SET status = 'queued', last_error = $2,
                run_at = now() + make_interval(secs => $3::float8),
                updated_at = now()
         WHERE id = $1",
    )
    .bind(job.id)
    .bind(&text)
    .bind(backoff_secs as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// The worker scheduling loop: while the number of running jobs is below the target
/// concurrency it keeps claiming (dispatching again the moment there is work), and polls
/// every 2s when idle; each job executes in its own tokio task, so a long extraction no
/// longer blocks syncing. `concurrency` is read hot every round -- changing the
/// concurrency in the system settings takes effect immediately.
/// The dispatch logic is injected by the caller as a handler (store does not depend on
/// the crates above it).
pub async fn run_worker<F, Fut>(pool: PgPool, concurrency: Arc<AtomicUsize>, handler: F)
where
    F: Fn(Job) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let running = Arc::new(AtomicUsize::new(0));
    // Orphan reclamation: when the process is killed nobody buries the running jobs, and
    // documents sit in extracting forever. In a single-process deployment, anything still
    // running at startup must be an orphan -- requeue them all (every handler is idempotent).
    match sqlx::query(
        "UPDATE jobs SET status = 'queued', locked_at = NULL, updated_at = now()
         WHERE status = 'running'",
    )
    .execute(&pool)
    .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::warn!(
                count = r.rows_affected(),
                "reclaimed orphaned jobs (they were running when the last process exited)"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "failed to reclaim orphaned jobs"),
    }
    tracing::info!(
        concurrency = concurrency.load(Ordering::Relaxed),
        "jobs worker started"
    );
    loop {
        let cap = concurrency.load(Ordering::Relaxed).max(1);
        if running.load(Ordering::Relaxed) >= cap {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        match claim_one(&pool).await {
            Ok(Some(job)) => {
                running.fetch_add(1, Ordering::Relaxed);
                let pool = pool.clone();
                let handler = handler.clone();
                let running = running.clone();
                tokio::spawn(async move {
                    let result = handler(job.clone()).await;
                    let outcome = match result {
                        Ok(()) => mark_done(&pool, job.id).await,
                        Err(e) => {
                            tracing::warn!(job_id = job.id, kind = %job.kind, error = %e, "job failed");
                            mark_failed(&pool, &job, &e).await
                        }
                    };
                    if let Err(e) = outcome {
                        tracing::error!(job_id = job.id, error = %e, "failed to write the job status back");
                    }
                    running.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Ok(None) => tokio::time::sleep(Duration::from_secs(2)).await,
            Err(e) => {
                tracing::error!(error = %e, "failed to claim a job, retrying in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::retry_delay;

    /// Backoff unchanged: 30s, 120s, 270s, and give up after the third.
    #[test]
    fn an_ordinary_failure_backs_off_and_then_gives_up() {
        assert_eq!(retry_delay(1, 3, false), Some(30));
        assert_eq!(retry_delay(2, 3, false), Some(120));
        assert_eq!(retry_delay(3, 3, false), None);
    }

    /// Marked hopeless means **it ends on the very first attempt** -- those three
    /// backoffs add up to seven minutes, and the balance will not grow back on its
    /// own inside seven minutes (#195).
    #[test]
    fn a_terminal_failure_does_not_spend_the_budget() {
        assert_eq!(retry_delay(1, 3, true), None);
    }
}
