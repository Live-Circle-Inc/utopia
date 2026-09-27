//! The query-the-data query engine: the trait seam (same technique as BlobStore) plus the
//! engine-agnostic safety gate.
//!
//! Engines are extended by protocol family, not by product name: the postgres wire protocol
//! (`postgres.rs`) -> the HTTP family -- `trino.rs` alone holds up the whole Iceberg / Delta /
//! Hive lakehouse ecosystem, while `databricks.rs` and `snowflake.rs` each go via their own SQL
//! REST API. The mount model and the registry are engine-agnostic, and adding an engine only
//! loosens one CHECK. The connection string is the only input: the engine is decided by the
//! scheme ([`engine_from_conn`]), each engine takes the rest of it apart itself (`conn.rs`), and
//! credentials only ever move around on the server side.
//!
//! The safety gates (defence in depth; the model is not trusted):
//! 1. sqlparser parses it: only a single SELECT/WITH gets through (CTEs included), and
//!    DML/DDL/multi-statement/SELECT INTO are rejected. The dialect is picked per engine;
//!    sqlparser has no Trino dialect, and Generic is a superset of it
//! 2. A LIMIT is forcibly wrapped around it (cap+1, to detect truncation)
//! 3. Session-level read-only + a statement timeout (each engine's own mechanism, so that even
//!    if the parser lets something slip it still cannot write). The HTTP family has no sessions,
//!    only statement timeouts -- read-only there rests on layer 1, and that is the one layer they
//!    have less of than the wire protocol
//! 4. Results are unified into JSON Lines: PG lets the database convert them itself; for the HTTP
//!    family the column names and values are assembled here, preserving column order

mod conn;
mod databricks;
mod postgres;
mod snowflake;
mod trino;

use sqlparser::ast::Statement;
use sqlparser::dialect::{DatabricksDialect, GenericDialect, PostgreSqlDialect, SnowflakeDialect};
use sqlparser::parser::Parser;
use std::time::Duration;

/// The row cap (a LIMIT of cap+1 is wrapped around it; row 201 is only used to decide whether
/// the result was truncated).
pub const ROW_CAP: usize = 200;
pub(crate) const STATEMENT_TIMEOUT_SECS: u32 = 10;
/// The HTTP family: the timeout on a single request, and the polling budget for one whole
/// statement from submission to having all the results
pub(crate) const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const HTTP_POLL_BUDGET: Duration = Duration::from_secs(30);

/// The values the `engine` column in the registry can take. The CHECK in the migration has to
/// agree with this table
pub const ENGINES: &[&str] = &["postgres", "trino", "databricks", "snowflake"];

#[derive(Debug)]
pub struct QueryResult {
    /// One JSON object text per row (key order = the query's column order)
    pub rows: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug)]
pub struct SchemaColumn {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub data_type: String,
    pub comment: Option<String>,
}

#[async_trait::async_trait]
pub trait QueryEngine: Send + Sync {
    async fn test(&self) -> anyhow::Result<()>;
    async fn fetch_schema(&self) -> anyhow::Result<Vec<SchemaColumn>>;
    /// Execute a SELECT that has been through the gate. The implementation itself still has to
    /// force a read-only session and a timeout (defence in depth).
    async fn execute(&self, sql: &str) -> anyhow::Result<QueryResult>;
}

/// scheme -> engine name. The UI has only one connection-string input box, and this is its only
/// dispatch point.
pub fn engine_from_conn(conn: &str) -> Option<&'static str> {
    let scheme = conn.trim().split("://").next()?.to_ascii_lowercase();
    match scheme.as_str() {
        "postgres" | "postgresql" => Some("postgres"),
        "trino" | "presto" => Some("trino"),
        "databricks" => Some("databricks"),
        "snowflake" => Some("snowflake"),
        _ => None,
    }
}

/// The engine factory. The credentials in conn only ever move around on the server side.
pub fn engine_for(engine: &str, conn: &str) -> anyhow::Result<Box<dyn QueryEngine>> {
    match engine {
        "postgres" => Ok(Box::new(postgres::PostgresEngine::new(conn))),
        "trino" => Ok(Box::new(trino::TrinoEngine::new(conn::TrinoConn::parse(
            conn,
        )?))),
        "databricks" => Ok(Box::new(databricks::DatabricksEngine::new(
            conn::DatabricksConn::parse(conn)?,
        ))),
        "snowflake" => Ok(Box::new(snowflake::SnowflakeEngine::new(
            conn::SnowflakeConn::parse(conn)?,
        ))),
        other => anyhow::bail!("Unsupported engine: {other}"),
    }
}

/// Safety gate, layer 1: parse and validate against the engine's dialect, and return the tidied
/// statement text.
pub fn guard_sql_for(engine: &str, sql: &str) -> anyhow::Result<String> {
    let cleaned = sql.trim().trim_end_matches(';').trim();
    if cleaned.is_empty() {
        anyhow::bail!("Empty SQL");
    }
    let parsed = match engine {
        "databricks" => Parser::parse_sql(&DatabricksDialect {}, cleaned),
        "snowflake" => Parser::parse_sql(&SnowflakeDialect {}, cleaned),
        "trino" => Parser::parse_sql(&GenericDialect {}, cleaned),
        _ => Parser::parse_sql(&PostgreSqlDialect {}, cleaned),
    };
    let statements = parsed.map_err(|e| anyhow::anyhow!("SQL parse error: {e}"))?;
    if statements.len() != 1 {
        anyhow::bail!("Exactly one statement is allowed");
    }
    match &statements[0] {
        Statement::Query(_) => Ok(cleaned.to_string()),
        other => anyhow::bail!(
            "Read-only: only SELECT/WITH queries are allowed (got {})",
            statement_kind(other)
        ),
    }
}

fn statement_kind(s: &Statement) -> &'static str {
    match s {
        Statement::Insert { .. } => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Delete { .. } => "DELETE",
        Statement::CreateTable { .. } => "CREATE TABLE",
        Statement::Drop { .. } => "DROP",
        Statement::AlterTable { .. } => "ALTER TABLE",
        Statement::Truncate { .. } => "TRUNCATE",
        _ => "a non-SELECT statement",
    }
}

/// Layer 2: wrap a LIMIT around it. All three HTTP engines accept this form; PG has its own
/// row_to_json version
pub(crate) fn wrap_limit(sql: &str) -> String {
    format!("SELECT * FROM ( {sql} ) AS _q LIMIT {}", ROW_CAP + 1)
}

/// Row 201 is only used to decide whether the result was truncated; it is not handed to the model
pub(crate) fn truncate_rows<T>(mut rows: Vec<T>) -> (Vec<T>, bool) {
    let truncated = rows.len() > ROW_CAP;
    rows.truncate(ROW_CAP);
    (rows, truncated)
}

/// Shared by the HTTP family: assembling "column names + row values" into JSON Lines. Done by
/// hand rather than with `serde_json::Map`, because without `preserve_order` that sorts by key,
/// whereas the column order is the order the query wrote down, and that is what the model reads
/// the table by
pub(crate) fn rows_to_json_lines(
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let mut line = String::from("{");
            for (i, col) in columns.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                line.push_str(&serde_json::to_string(col).unwrap_or_else(|_| "\"?\"".into()));
                line.push(':');
                let value = row.get(i).cloned().unwrap_or(serde_json::Value::Null);
                line.push_str(&value.to_string());
            }
            line.push('}');
            line
        })
        .collect()
}

/// Databricks's JSON_ARRAY and Snowflake's data hand every value over as a string (or null).
/// Restore numbers and booleans according to the column type, and leave the rest as strings --
/// a model does different arithmetic on `"42"` than on `42`
pub(crate) fn coerce(type_name: &str, raw: &serde_json::Value) -> serde_json::Value {
    let serde_json::Value::String(s) = raw else {
        return raw.clone();
    };
    let ty = type_name.to_ascii_uppercase();
    const NUMERIC: &[&str] = &[
        "INT", "LONG", "SHORT", "BYTE", "FLOAT", "DOUBLE", "DECIMAL", "NUMBER", "FIXED", "REAL",
        "NUMERIC",
    ];
    // INTERVAL contains "INT" as well: if it does not parse as a number it is left as it was,
    // so there is no collateral damage
    if NUMERIC.iter().any(|k| ty.contains(k)) {
        if let Ok(n) = s.parse::<i64>() {
            return n.into();
        }
        if let Ok(f) = s.parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return serde_json::Value::Number(n);
            }
        }
    }
    if ty.starts_with("BOOL") {
        match s.as_str() {
            "true" | "TRUE" => return true.into(),
            "false" | "FALSE" => return false.into(),
            _ => {}
        }
    }
    raw.clone()
}

/// Escaping for single-quoted literals: the schema name goes into information_schema's WHERE
/// clause
pub(crate) fn sql_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The client shared by the HTTP family.
///
/// **The proxy policy is explicit**: loopback addresses and the hosts in `NO_PROXY` connect
/// directly, and everything else goes via `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY`. reqwest's
/// system-proxy detection is not used -- on Windows it reads the registry, and it does not
/// recognise all the bypass forms written there such as `127.*`, so a stand-in service on the
/// local machine gets sent through the proxy and comes back a 502. A service process should be
/// looking at environment variables, and this rule matches how it is written in docker-compose
pub(crate) fn http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(HTTP_REQUEST_TIMEOUT)
        .user_agent("utopia")
        .proxy(reqwest::Proxy::custom(|url: &reqwest::Url| proxy_for(url)))
        .build()?)
}

fn proxy_for(url: &reqwest::Url) -> Option<reqwest::Url> {
    let host = url.host_str()?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if loopback || no_proxy_matches(host) {
        return None;
    }
    let keys: &[&str] = if url.scheme() == "https" {
        &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
    };
    keys.iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| reqwest::Url::parse(v.trim()).ok())
}

/// The common forms of `NO_PROXY=localhost,127.0.0.1,.internal,corp.example`: whole-name
/// equality, or a suffix match for entries starting with a dot
fn no_proxy_matches(host: &str) -> bool {
    let raw = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    raw.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "*")
        .any(|p| {
            let p = p.trim_start_matches('.');
            host.eq_ignore_ascii_case(p)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", p.to_ascii_lowercase()))
        })
        || raw.split(',').any(|p| p.trim() == "*")
}

#[cfg(test)]
mod tests {
    use super::{coerce, engine_from_conn, guard_sql_for, rows_to_json_lines};
    use serde_json::json;

    fn guard_sql(sql: &str) -> anyhow::Result<String> {
        guard_sql_for("postgres", sql)
    }

    #[test]
    fn allows_select_and_cte() {
        assert!(guard_sql("SELECT region, sum(amount) FROM orders GROUP BY 1").is_ok());
        assert!(guard_sql("WITH t AS (SELECT 1 AS x) SELECT * FROM t;").is_ok());
    }

    #[test]
    fn rejects_writes_and_ddl() {
        for bad in [
            "UPDATE orders SET amount = 0",
            "DELETE FROM orders",
            "INSERT INTO orders (region) VALUES ('east')",
            "DROP TABLE orders",
            "TRUNCATE orders",
            "CREATE TABLE t (id int)",
            "ALTER TABLE orders ADD COLUMN x int",
        ] {
            assert!(guard_sql(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn rejects_multi_statement() {
        assert!(guard_sql("SELECT 1; DROP TABLE orders").is_err());
        assert!(guard_sql("").is_err());
    }

    #[test]
    fn every_dialect_keeps_the_same_gate() {
        for engine in ["postgres", "trino", "databricks", "snowflake"] {
            assert!(
                guard_sql_for(engine, "SELECT a FROM t WHERE b > 1").is_ok(),
                "{engine}"
            );
            assert!(guard_sql_for(engine, "DELETE FROM t").is_err(), "{engine}");
            assert!(
                guard_sql_for(engine, "SELECT 1; SELECT 2").is_err(),
                "{engine}"
            );
        }
        // Each vendor's dialect details -- backticks, double-colon casts -- all have to get
        // through
        assert!(guard_sql_for("databricks", "SELECT `region` FROM main.sales.orders").is_ok());
        assert!(guard_sql_for("snowflake", "SELECT amount::number FROM db.public.orders").is_ok());
        assert!(guard_sql_for("trino", "SELECT count(*) FROM hive.default.orders").is_ok());
    }

    #[test]
    fn engine_follows_the_scheme() {
        assert_eq!(engine_from_conn("postgres://u:p@h/db"), Some("postgres"));
        assert_eq!(engine_from_conn("postgresql://u:p@h/db"), Some("postgres"));
        assert_eq!(engine_from_conn("trino://u@h:8443/hive"), Some("trino"));
        assert_eq!(engine_from_conn("presto://u@h/hive"), Some("trino"));
        assert_eq!(
            engine_from_conn("databricks://:t@h/sql/1.0/warehouses/x"),
            Some("databricks")
        );
        assert_eq!(
            engine_from_conn("snowflake://:t@a.snowflakecomputing.com/db"),
            Some("snowflake")
        );
        assert_eq!(engine_from_conn("mysql://u@h/db"), None);
        assert_eq!(engine_from_conn("garbage"), None);
    }

    #[test]
    fn json_lines_keep_column_order() {
        let cols = vec!["zeta".to_string(), "alpha".to_string()];
        let rows = vec![vec![json!(1), json!("x")], vec![json!(null)]];
        assert_eq!(
            rows_to_json_lines(&cols, &rows),
            vec![r#"{"zeta":1,"alpha":"x"}"#, r#"{"zeta":null,"alpha":null}"#]
        );
    }

    #[test]
    fn strings_come_back_as_numbers_when_the_column_says_so() {
        assert_eq!(coerce("DOUBLE", &json!("12.5")), json!(12.5));
        assert_eq!(coerce("fixed", &json!("42")), json!(42));
        assert_eq!(coerce("BOOLEAN", &json!("true")), json!(true));
        assert_eq!(coerce("STRING", &json!("42")), json!("42"));
        assert_eq!(coerce("INTERVAL", &json!("1 day")), json!("1 day"));
        assert_eq!(coerce("DOUBLE", &json!(null)), json!(null));
    }
}
