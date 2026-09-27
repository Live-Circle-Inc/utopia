//! Alert centre (0005). **Review is about whether the knowledge is correct; alerts are about
//! whether the system is alive.**
//!
//! **One failure, one row, never edited again.** This table deliberately has no state machine:
//! no "resolved", no self-healing, no collapsing several failures into a single row.
//!
//! It did once, and the price was that every new alert kind had to implement "what counts as
//! fixed" all over again -- `source.sync_failed` has a natural success signal (the sync
//! succeeded), `llm.unreachable` does not, so it needed a background probe built just for it;
//! a third alert kind would need a third mechanism, and a forgotten clear is invisible at
//! compile time, the symptom being an alert that stays lit forever. More fundamentally,
//! **that is not the question the alert centre should be answering**: whether something is
//! still broken right now is written on the source page and in the document status. An alert's
//! job is to make someone go take a look, not to be a live dashboard.
//!
//! No dedup either. "The same source failing every hour for 24 hours straight" is 24 rows, not
//! 1 -- writing it as 1 would mean deciding "is this a recurrence or has it just never
//! recovered", and that is **indistinguishable in the data** unless you bring in a clock or a
//! success signal. A missed alert costs far more than a row does, so we would rather write the
//! extra rows and let [`purge_older_than`] clean up.

use sqlx::PgPool;
use utopia_core::models::{Role, User};
use utopia_core::AppResult;
use uuid::Uuid;

/// Alert kinds. **The strings are pinned here rather than scattered across call sites**: the UI
/// looks up its wording by them, so one mistyped letter falls back to showing the raw code, and
/// that kind of mistake is invisible at compile time.
pub mod kind {
    /// KB-level: some source failed to sync. `min_role = editor`
    pub const SOURCE_SYNC_FAILED: &str = "source.sync_failed";
    /// System-level: the model endpoint gave no usable answer -- unreachable, or reachable but
    /// what came back is not this API. A clean 4xx from the endpoint does not count: that means
    /// it really is the model API, just with the wrong key or quota.
    /// For the quota case see [`LLM_RATE_LIMITED`]
    pub const LLM_UNREACHABLE: &str = "llm.unreachable";
    /// System-level: the endpoint is rate limiting and it still will not go through after the
    /// backoff retries are exhausted. `min_role = admin`
    ///
    /// **Kept separate from [`LLM_UNREACHABLE`] because what you have to do differs**: an
    /// unreachable endpoint means going and checking the network or the address, a maxed-out
    /// quota means lowering concurrency or upgrading the plan -- different people, different
    /// actions.
    ///
    /// severity is `warning`, not `error`: quota recovers on its own, a dead endpoint does not.
    ///
    /// This fills in the half that "backoff retry" left behind. After retrying we no longer
    /// lose data, but when a document really does get shut out by the quota, without this alert
    /// nobody knows at all -- in one measured run 4 documents failed, half of them unreachable
    /// (alerted) and half of them rate limited (silent).
    pub const LLM_RATE_LIMITED: &str = "llm.rate_limited";
    /// System-level: the account cannot pay for requests -- overdue balance or the plan's quota
    /// is used up. `min_role = admin`
    ///
    /// **`error` rather than `warning`, precisely because the difference from rate limiting is
    /// "will it get better on its own"**: quota resets on schedule, an overdue balance does not.
    /// Until somebody tops up, extraction and vectorisation on this deployment stay stopped.
    pub const LLM_OUT_OF_CREDIT: &str = "llm.out_of_credit";
    /// KB-level: the data source got mounted, but its schema was never ingested. `min_role = admin`
    ///
    /// **This one describes not that failure but the state it left behind**: the source is
    /// mounted, yet data questions cannot see which tables it has -- `query_data` still gets
    /// queued, but the model can only guess at column names. The error at the moment of
    /// mounting is visible only to the person who clicked the button, and from then on this KB
    /// just stays silently incomplete.
    pub const SCHEMA_SYNC_FAILED: &str = "data_source.schema_sync_failed";
    /// KB-level: mapping exploration finished and did not propose a single definition.
    /// `severity = info`, `min_role = editor` -- not a failure, but "the thing you were waiting
    /// for produced no result", and there is nowhere else on the page that can say that (#223)
    pub const MAPPING_EXPLORATION_EMPTY: &str = "mapping.exploration_empty";
}

/// One failure. Packing it into a struct is not just about the argument count -- writing
/// `severity: "error"` at the call site reads far better than "the third positional argument is
/// error", and the number of alert sources will only grow.
pub struct NewAlert<'a> {
    /// None = system-level
    pub kb_id: Option<Uuid>,
    pub severity: &'a str,
    /// Use the constants in [`kind`], do not write literals
    pub kind: &'a str,
    pub min_role: Role,
    /// document / source / system
    pub subject_type: Option<&'a str>,
    pub subject_id: Option<Uuid>,
    /// The part people look at: names, the original error text. **The name has to be stored
    /// here** -- once the object is deleted `subject_id` no longer resolves to a name, and an
    /// alert should outlast it
    pub detail: serde_json::Value,
}

/// Record one failure. Just an INSERT: no conflict handling, no read-back.
pub async fn raise(pool: &PgPool, a: NewAlert<'_>) -> AppResult<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO alerts
             (id, kb_id, severity, kind, min_role, subject_type, subject_id, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(a.kb_id)
    .bind(a.severity)
    .bind(a.kind)
    .bind(a.min_role.as_str())
    .bind(a.subject_type)
    .bind(a.subject_id)
    .bind(a.detail)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Retention cleanup. **This is exactly where the purely atomic design costs you**: a broken
/// source syncing hourly writes 24 rows a day; without cleanup this table grows into a second
/// log file.
///
/// Read receipts go with it (foreign key CASCADE).
pub async fn purge_older_than(pool: &PgPool, days: i32) -> AppResult<u64> {
    let n = sqlx::query("DELETE FROM alerts WHERE created_at < now() - make_interval(days => $1)")
        .bind(days)
        .execute(pool)
        .await?;
    Ok(n.rows_affected())
}

/// The visibility predicate, **written exactly once**. The list, the unread count and
/// mark-all-read each run their own query, but "who can see what" is one single rule; copy it
/// three times and it will drift sooner or later.
///
/// `$1` = user_id, `$2` = is_admin, `$3` = the array of visible kbs, `$4` = the rank of the
/// matching role.
const VISIBLE: &str = "
    CASE
        WHEN a.kb_id IS NULL THEN $2::bool
        ELSE EXISTS (
            SELECT 1 FROM unnest($3::uuid[], $4::int[]) AS v(kb, rank)
            WHERE v.kb = a.kb_id
              AND v.rank >= CASE a.min_role
                    WHEN 'viewer' THEN 0
                    WHEN 'editor' THEN 1
                    WHEN 'admin'  THEN 2
                    ELSE 3 END)
    END";

/// Search matches the **KB name, the subject detail and the kind code**, not the headline shown
/// in the UI.
///
/// The headline's wording lives in the client (0004: the server produces no display copy), so
/// the server cannot search it. This is not a compromise: what people go looking for is a source
/// name, a KB name, the original error text -- those are language-neutral, and they are right
/// there in detail. Finding things by category is what a filter is for, not the search box.
const SEARCH: &str = "
    ($5::text IS NULL
     OR a.kind ILIKE '%' || $5 || '%'
     OR COALESCE(k.name, '') ILIKE '%' || $5 || '%'
     OR a.detail::text ILIKE '%' || $5 || '%')";

/// A group: several **consecutive** failures with the same `(kb, kind)`.
///
/// Storage is atomic (one failure, one row); collapsing only affects reads. Grouping happens on
/// the server rather than the front end because **pagination has to be by group**: if the front
/// end collapsed, a page could only fetch a fixed number of rows, so a run of consecutive
/// failures that straddles a page boundary would break into two groups, and one click would only
/// mark up to that boundary.
#[derive(sqlx::FromRow)]
pub struct AlertGroup {
    pub kb_id: Option<Uuid>,
    pub kb_name: Option<String>,
    pub kind: String,
    /// The heaviest severity in the group
    pub severity: String,
    /// How many times in this group
    pub count: i64,
    /// How many of those I have not read
    pub unread: i64,
    /// The newest and oldest timestamps in the group. **Mark-as-read fences by this range**
    /// instead of sending the front end an id list -- a group can hold hundreds of rows
    pub latest_at: chrono::DateTime<chrono::Utc>,
    pub earliest_at: chrono::DateTime<chrono::Utc>,
    /// Details, at most [`GROUP_LINES`] of them, newest first
    pub lines: Vec<serde_json::Value>,
}

/// How many detail lines a group brings back at most. The panel cannot list any more than that,
/// and a group can hold hundreds -- sending them all just makes the first paint slower
const GROUP_LINES: i64 = 5;

/// One page of groups, plus the total group count.
pub struct GroupPage {
    pub items: Vec<AlertGroup>,
    pub total: i64,
}

/// Collapsing adjacent same-kind rows: subtracting two `row_number()`s (gaps and islands).
///
/// The global sequence number minus "the sequence number within the same (kb, kind)" gives the
/// same difference for consecutive rows of the same kind, and another failure slipping in between
/// makes that difference change -- so the difference *is* the group number.
/// `PARTITION BY kb_id` treats NULL as equal, so system-level alerts naturally fall into one
/// group.
const ISLANDS: &str = "
    SELECT v.*,
           row_number() OVER (ORDER BY v.created_at DESC, v.id DESC)
         - row_number() OVER (PARTITION BY v.kb_id, v.kind
                              ORDER BY v.created_at DESC, v.id DESC) AS grp
    FROM v";

/// The alerts this person can see, **paginated by group**, newest first.
pub async fn list_groups(
    pool: &PgPool,
    user: &User,
    q: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<GroupPage> {
    let (kb_ids, kb_roles) = visible(pool, user).await?;
    // An empty string counts as no search: clearing the search box should not turn into
    // "search for an empty string"
    let q = q.map(str::trim).filter(|s| !s.is_empty());
    let base = format!(
        "WITH v AS (
             SELECT a.id, a.kb_id, k.name AS kb_name, a.severity, a.kind,
                    a.detail, a.created_at, (r.user_id IS NOT NULL) AS read
             FROM alerts a
             LEFT JOIN knowledge_bases k ON k.id = a.kb_id
             LEFT JOIN alert_reads r ON r.alert_id = a.id AND r.user_id = $1
             WHERE ({VISIBLE}) AND ({SEARCH})
         ),
         isl AS ({ISLANDS})"
    );
    let sql = format!(
        "{base}
         SELECT kb_id, max(kb_name) AS kb_name, kind,
                -- severity takes the max by seriousness, not lexicographically: that way
                -- warning would outrank error
                CASE max(CASE severity WHEN 'error' THEN 3 WHEN 'warning' THEN 2 ELSE 1 END)
                    WHEN 3 THEN 'error' WHEN 2 THEN 'warning' ELSE 'info' END AS severity,
                count(*) AS count,
                count(*) FILTER (WHERE NOT read) AS unread,
                max(created_at) AS latest_at,
                min(created_at) AS earliest_at,
                (array_agg(detail ORDER BY created_at DESC))[1:{GROUP_LINES}] AS lines
         FROM isl
         GROUP BY kb_id, kind, grp
         ORDER BY max(created_at) DESC
         LIMIT $6 OFFSET $7"
    );
    let items: Vec<AlertGroup> = sqlx::query_as(&sql)
        .bind(user.id)
        .bind(user.is_admin)
        .bind(&kb_ids)
        .bind(&kb_roles)
        .bind(q)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    // The total number of **groups**, not of rows -- the pager counts groups
    let count_sql =
        format!("{base} SELECT count(*) FROM (SELECT 1 FROM isl GROUP BY kb_id, kind, grp) g");
    let (total,): (i64,) = sqlx::query_as(&count_sql)
        .bind(user.id)
        .bind(user.is_admin)
        .bind(&kb_ids)
        .bind(&kb_roles)
        .bind(q)
        .fetch_one(pool)
        .await?;
    Ok(GroupPage { items, total })
}

/// Mark a whole group read. **Fenced by time range**, not by id list -- a group can hold
/// hundreds of rows, and sending all the ids to the front end and back again is a wasted trip.
///
/// Visibility is still checked: nobody gets to guess a kind and mark away alerts they cannot see.
pub async fn mark_group_read(
    pool: &PgPool,
    user: &User,
    kb_id: Option<Uuid>,
    kind: &str,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> AppResult<u64> {
    let (kb_ids, kb_roles) = visible(pool, user).await?;
    let sql = format!(
        "INSERT INTO alert_reads (alert_id, user_id)
         SELECT a.id, $1 FROM alerts a
         WHERE ({VISIBLE})
           AND a.kind = $5
           -- IS NOT DISTINCT FROM: a system-level alert has a NULL kb_id, which = cannot compare
           AND a.kb_id IS NOT DISTINCT FROM $6
           AND a.created_at BETWEEN $7 AND $8
         ON CONFLICT DO NOTHING"
    );
    let n = sqlx::query(&sql)
        .bind(user.id)
        .bind(user.is_admin)
        .bind(&kb_ids)
        .bind(&kb_roles)
        .bind(kind)
        .bind(kb_id)
        .bind(from)
        .bind(to)
        .execute(pool)
        .await?;
    Ok(n.rows_affected())
}

/// My unread count.
pub async fn unread_count(pool: &PgPool, user: &User) -> AppResult<i64> {
    let (kb_ids, kb_roles) = visible(pool, user).await?;
    let sql = format!(
        "SELECT count(*) FROM alerts a
         LEFT JOIN alert_reads r ON r.alert_id = a.id AND r.user_id = $1
         WHERE ({VISIBLE}) AND r.user_id IS NULL"
    );
    let (n,): (i64,) = sqlx::query_as(&sql)
        .bind(user.id)
        .bind(user.is_admin)
        .bind(&kb_ids)
        .bind(&kb_roles)
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Mark everything I can see as read.
///
/// One SQL statement rather than inserting row by row: row by row would mean fetching the list
/// first, and "what can be seen" is already written out once in [`VISIBLE`], so fetching it and
/// iterating means using the same rule twice.
pub async fn mark_all_read(pool: &PgPool, user: &User) -> AppResult<u64> {
    let (kb_ids, kb_roles) = visible(pool, user).await?;
    let sql = format!(
        "INSERT INTO alert_reads (alert_id, user_id)
         SELECT a.id, $1 FROM alerts a
         WHERE ({VISIBLE})
         ON CONFLICT DO NOTHING"
    );
    let n = sqlx::query(&sql)
        .bind(user.id)
        .bind(user.is_admin)
        .bind(&kb_ids)
        .bind(&kb_roles)
        .execute(pool)
        .await?;
    Ok(n.rows_affected())
}

/// The visible KBs split into two parallel arrays: Postgres has no convenient syntax for an
/// array of tuples, and expanding two columns side by side with `unnest(a, b)` is the standard
/// approach.
async fn visible(pool: &PgPool, user: &User) -> AppResult<(Vec<Uuid>, Vec<i32>)> {
    let roles = crate::access::visible_kb_roles(pool, user).await?;
    Ok(roles.into_iter().map(|(id, r)| (id, rank(r))).unzip())
}

/// The ordering of roles, used to compare against `alerts.min_role`.
/// Same order as [`Role`]'s `PartialOrd`, and the same order as that CASE in [`VISIBLE`] --
/// all three have to agree
fn rank(r: Role) -> i32 {
    match r {
        Role::Viewer => 0,
        Role::Editor => 1,
        Role::Admin => 2,
        Role::Owner => 3,
    }
}
