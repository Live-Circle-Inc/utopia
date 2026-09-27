#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("Not found")]
    NotFound,
    #[error("Not signed in or invalid credentials")]
    Unauthorized,
    #[error("You don't have permission to do that")]
    Forbidden,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Validation(String),
    /// A validation error carrying a stable code. **The message is still the original English
    /// sentence** -- it is there for the clients that do not localise (MCP, CLI) and for the
    /// logs; the UI takes the code and looks the wording up in i18n.
    ///
    /// Now that the UI language lives in the client, the backend no longer owns a locale (see
    /// docs/decisions/0004), so the strings left here are permanently English. Anything a user
    /// can run into should carry a code.
    #[error("{message}")]
    Invalid {
        code: &'static str,
        message: String,
        /// Detail supplied by a machine (the cron parser's complaint, say). The wording
        /// belongs to the UI, the details belong here
        detail: Option<String>,
    },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl AppError {
    pub fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        AppError::Invalid {
            code,
            message: message.into(),
            detail: None,
        }
    }
    pub fn invalid_detail(
        code: &'static str,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        AppError::Invalid {
            code,
            message: message.into(),
            detail: Some(detail.into()),
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// Marked on a failure that **will not get better by retrying** (see issue #195).
///
/// The queue's default assumption is "maybe it will be fine if we wait a bit", and most
/// failures really are like that: an endpoint stutters, the database is busy for an instant, a
/// rate limit clears within a minute. Running out of credit is not -- three retries spaced 30
/// seconds, 2 minutes and 4.5 minutes apart, nobody is going to top up the balance inside those
/// seven minutes, the retries just say the same error three times over, and the one "failed"
/// that ops needs to see is pushed back by seven minutes before it shows up.
///
/// **The judgement stays on the handler's side, not in the queue.** What counts as hopeless
/// depends on the domain -- `utopia-store` cannot see `utopia-llm`'s error types, and should
/// not be able to. The handler attaches this marker (`err.context(Terminal)`), and the queue
/// only asks "is it attached".
///
/// Attaching it changes nothing else: alerts still fire (`observe_job_failure` honours this
/// marker as well, otherwise failing faster would mean nobody is told at all), and
/// `last_error` is still written.
#[derive(Debug, Clone, Copy)]
pub struct Terminal;

impl std::fmt::Display for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("will not recover by retrying")
    }
}

impl std::error::Error for Terminal {}

/// Was this failure marked as not worth retrying? **Look along the whole context chain** --
/// after the handler attaches the marker the layers above keep calling `context(...)`, so
/// looking only at the outermost one is the same as not looking
pub fn is_terminal(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<Terminal>())
}
