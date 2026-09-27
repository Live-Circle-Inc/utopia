//! The entry point for database-backed integration tests (#248).
//!
//! Every database-backed test opens with the same line: with no `UTOPIA_DATABASE_URL`, skip
//! rather than fail, so a casual local `cargo test` does not require standing a database up
//! first. But if CI skips the same way, the green becomes a lie: the backend job has no database,
//! all 24 store integration tests silently return, and the migrations job that does have a
//! database only ran one of them.
//!
//! So the skip has to depend on the setting: where `UTOPIA_TEST_REQUIRE_DB` is set (CI's
//! database-backed job), having no database is a failure -- "what should have run didn't" has to
//! be visible.

/// The database URL for database-backed tests. `None` = skip this time.
///
/// Panics when `UTOPIA_TEST_REQUIRE_DB` is set but there is no URL: this is for CI -- a skip
/// there means the tests never executed at all, and that must not show up green
pub fn url() -> Option<String> {
    match std::env::var("UTOPIA_DATABASE_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u),
        _ => {
            if std::env::var_os("UTOPIA_TEST_REQUIRE_DB").is_some() {
                panic!(
                    "UTOPIA_TEST_REQUIRE_DB is set but UTOPIA_DATABASE_URL is not:                      this run must not skip database-backed tests"
                );
            }
            eprintln!(
                "skipping: UTOPIA_DATABASE_URL is not set (set UTOPIA_TEST_REQUIRE_DB=1 to turn a skip into a failure)"
            );
            None
        }
    }
}
