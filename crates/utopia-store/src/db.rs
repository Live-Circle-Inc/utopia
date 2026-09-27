use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;

/// The default ceiling for the connection pool.
///
/// **It is no longer equal to the worker concurrency** (the worker default is already 64, see
/// migration 0011), and that is deliberate: background tasks spend most of their time waiting for
/// a model to answer, and during that time they neither hold a connection nor escape the
/// per-model semaphore, which keeps them down to a dozen or so. What the pool has to cover is the
/// ones **actually doing work** -- short queries like the per-chunk epoch check, vector
/// retrieval, and unmatched counts, which come in bursts.
///
/// This was once hardcoded to 10 while the worker defaulted to 32 -- threefold oversubscription,
/// and when it hit, requests got slower first and then timed out, rather than anything anywhere
/// reporting "the pool is too small". So the criterion for this number is "how many short queries
/// are running at once", not "how many task slots there are"; when the worker is raised further,
/// the former is what to weigh.
const DEFAULT_MAX_CONNECTIONS: u32 = 32;

pub async fn connect(database_url: &str, max_connections: Option<u32>) -> anyhow::Result<PgPool> {
    let max = max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS).max(2);
    let pool = PgPoolOptions::new()
        .max_connections(max)
        // fail early and loudly when a connection can't be acquired, instead of leaving the
        // request hanging for the default 30 seconds -- if the pool is sized too small, it has to
        // be visible that the pool is the problem
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await?;
    tracing::info!(
        max_connections = max,
        "database connection pool established"
    );
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}
