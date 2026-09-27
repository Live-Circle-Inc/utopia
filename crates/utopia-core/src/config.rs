use figment::{
    providers::{Env, Serialized},
    Figment,
};
use serde::{Deserialize, Serialize};

/// Global configuration. Source precedence: environment variables (prefix `UTOPIA_`) > defaults.
/// The `.env` file is preloaded into the environment by the binary entry point via dotenvy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub database_url: String,
    /// The connection string used to run migrations. Migrations create tables and triggers and
    /// the runtime does not need those privileges -- once the two are separate, the application
    /// can connect as a restricted role that only reads and writes the business tables and can
    /// only append to, never modify, the ledger.
    /// Unset, it falls back to `database_url`: an existing deployment upgrades as usual with no
    /// changes.
    pub migration_url: Option<String>,
    pub bind_addr: String,
    /// The JWT signing secret. Left empty, it is generated on first boot and stored in
    /// deployment_settings -- asking the deployer to fill in a random string by hand means that
    /// in reality the default value goes to production as-is.
    /// Given explicitly, it wins over the one in the database: that is the path for key
    /// rotation and for lining several instances up explicitly.
    pub jwt_secret: Option<String>,
    /// The frontend build output directory; when it exists the server hosts the SPA (history
    /// fallback).
    pub web_dist: String,
    /// The data directory: the raw files (files/) and the Tantivy index (index/).
    pub data_dir: String,
    /// The database connection pool ceiling. Defaults to 32, lined up with the default worker
    /// concurrency -- when the pool is smaller than the concurrency the symptom is that
    /// requests get slower, not that anywhere says "the pool is too small", so it has to be
    /// tunable.
    pub db_max_connections: Option<u32>,
    /// Force the Secure flag onto the session cookie. Defaults to false: the request's
    /// X-Forwarded-Proto decides, and the flag is only set over TLS. You only need to force it
    /// on here when the proxy does not send that header.
    pub cookie_secure: bool,
    /// Whether registration is open. When false only the first user (bootstrapping the
    /// deployment) can register, and everyone else needs an admin to open it.
    pub open_registration: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            database_url: "postgres://utopia:utopia@localhost:1517/utopia".into(),
            migration_url: None,
            bind_addr: "0.0.0.0:1516".into(),
            jwt_secret: None,
            web_dist: "web/dist".into(),
            data_dir: "data".into(),
            db_max_connections: None,
            cookie_secure: false,
            open_registration: true,
        }
    }
}

impl AppConfig {
    pub fn load() -> anyhow::Result<Self> {
        let cfg = Figment::from(Serialized::defaults(AppConfig::default()))
            .merge(Env::prefixed("UTOPIA_"))
            .extract()?;
        Ok(cfg)
    }
}

impl AppConfig {
    /// The migration connection string: the runtime one when nothing separate is configured.
    pub fn migration_url(&self) -> &str {
        self.migration_url.as_deref().unwrap_or(&self.database_url)
    }
}
