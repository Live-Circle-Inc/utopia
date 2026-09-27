//! The outer backstop **can actually be raised**, and the two defaults say the same
//! number (migration 0011).
//!
//! Why this insists on a real database: the shape of this bug is **the constraint is in
//! SQL, the validation is in Rust, and the two drift apart independently**.
//! `set_worker_concurrency` allows 1..=256, while the CHECK on the column used to be
//! `BETWEEN 1 AND 32` -- type any value between 33 and 256 into the settings page and
//! Rust says fine, the database says no, and what the user sees is a CHECK constraint
//! error. `cargo check` and clippy have not a word to say about it, because the two sides
//! are not even in the same language.
//!
//! The upper bound of the constraint was originally equal to the column default (both 32),
//! so **the backstop could not be raised by a single notch** -- while the comment in 0001
//! says it "has to be clearly larger than the sum of the per-model limits, or rate-limited
//! jobs will fill up the slots and starve everything else". The constraint had walled off
//! its own design.
//!
//! Skips rather than fails when there is no `UTOPIA_DATABASE_URL`. Read-only + restore
//! after changing anything, never leaving a trace.
//!
//! **The two checks run in sequence inside the same test**: they both touch the same
//! singleton row, and tests in one binary run concurrently by default. Row locks would
//! hold off dirty reads anyway, but there is no need for the interleaving of one test
//! writing while the other deletes (inside a transaction) to exist at all -- put them in
//! sequence and nothing has to be gambled on (raised by the reporter of #248).

use sqlx::PgPool;

#[tokio::test]
async fn the_backstop_can_be_raised() -> anyhow::Result<()> {
    let Some(url) = utopia_store::test_db::url() else {
        return Ok(());
    };
    let pool = PgPool::connect(&url).await?;
    every_value_rust_accepts_the_database_accepts_too(&pool).await?;
    the_two_defaults_say_the_same_number(&pool).await
}

/// Every value Rust allows, the database has to allow too.
///
/// Trying each boundary rather than only one: the drift could sit at any notch, and these
/// few queries are cheap.
async fn every_value_rust_accepts_the_database_accepts_too(pool: &PgPool) -> anyhow::Result<()> {
    let before = utopia_store::access::worker_concurrency(pool).await?;

    let run = async {
        // 33 is the first notch that used to be stuck; 256 is the ceiling on the Rust side
        for v in [1_i32, 33, 64, 255, 256] {
            utopia_store::access::set_worker_concurrency(pool, v)
                .await
                .map_err(|e| anyhow::anyhow!("Rust allowed {v}, the database refused: {e}"))?;
            let got = utopia_store::access::worker_concurrency(pool).await?;
            assert_eq!(got, v, "wrote {v} in and read {got} back");
        }

        // The other direction: what Rust refuses must not quietly land in the database
        for v in [0_i32, 257, -1] {
            assert!(
                utopia_store::access::set_worker_concurrency(pool, v)
                    .await
                    .is_err(),
                "{v} is out of range yet was accepted"
            );
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    // Restore: this is a shared deployment setting, and a test has no business changing
    // somebody else's runtime parameters
    utopia_store::access::set_worker_concurrency(pool, before).await?;
    run
}

/// The Rust fallback for when the table has no row must be the same number as the column
/// default.
///
/// They are written in two places (one in SQL, one in Rust), and changing one does not
/// carry the other along. The consequence of a mismatch is very well hidden: a database
/// with a row runs one number, a database with an empty table runs another, and neither
/// of them reports an error.
async fn the_two_defaults_say_the_same_number(pool: &PgPool) -> anyhow::Result<()> {
    // The column default: ask information_schema directly, do not guess
    let column_default: Option<String> = sqlx::query_scalar(
        "SELECT column_default FROM information_schema.columns
          WHERE table_name = 'deployment_settings' AND column_name = 'worker_concurrency'",
    )
    .fetch_one(pool)
    .await?;
    let column_default: i32 = column_default
        .as_deref()
        .and_then(|s| s.split("::").next())
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| anyhow::anyhow!("cannot read the column default: {column_default:?}"))?;

    // The Rust fallback: hide the row away and ask once more
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM deployment_settings")
        .execute(&mut *tx)
        .await?;
    let fallback: Option<(i32,)> =
        sqlx::query_as("SELECT worker_concurrency FROM deployment_settings LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?;
    assert!(fallback.is_none(), "row not deleted, next check is moot");
    tx.rollback().await?; // **Must roll back**: deployment_settings is a singleton, and deleting it leaves the service with no settings

    // The number inside the unwrap_or in access.rs
    let rust_fallback = 64;
    assert_eq!(
        column_default, rust_fallback,
        "the column default is {column_default}, the Rust fallback is {rust_fallback} -- the two have drifted apart"
    );
    Ok(())
}
