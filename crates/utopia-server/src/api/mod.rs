mod admin_routes;
mod alerts_routes;
mod auth_routes;
mod chat;
mod datasource_routes;
mod documents_routes;
mod events_routes;
mod graph_routes;
mod jobs_routes;
mod kbs;
mod mapping_routes;
mod mcp;
mod members_routes;
pub(crate) mod ontology_routes;
mod review_routes;
mod search_routes;
mod settings_routes;
mod sources_routes;
mod token_routes;
mod tools;
mod workspaces;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderValue, Method};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use utopia_core::config::AppConfig;

use crate::state::AppState;

const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

pub fn router(state: AppState, cfg: &AppConfig) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/auth/register", post(auth_routes::register))
        .route("/auth/login", post(auth_routes::login))
        .route("/auth/logout", post(auth_routes::logout))
        // Alert centre: cross-KB, so it does not hang under /kbs/{id}
        .route("/alerts", get(alerts_routes::list))
        .route("/alerts/unread", get(alerts_routes::unread))
        .route("/alerts/read-all", post(alerts_routes::mark_all_read))
        .route("/alerts/read-group", post(alerts_routes::mark_group_read))
        .route("/alerts/events", get(alerts_routes::stream))
        .route(
            "/auth/me",
            get(auth_routes::me).patch(auth_routes::update_me),
        )
        .route("/auth/password", post(auth_routes::change_password))
        .route(
            "/workspaces",
            get(workspaces::list).post(workspaces::create),
        )
        .route(
            "/workspaces/{id}",
            get(workspaces::get_one)
                .patch(workspaces::rename)
                .delete(workspaces::delete),
        )
        .route("/workspaces/{id}/kbs", get(kbs::list).post(kbs::create))
        // Static list for the create-KB screen; nothing to do with any particular workspace
        .route("/ontology-packs", get(kbs::list_packs))
        .route("/workspaces/{id}/my-kbs", get(kbs::my_kbs))
        .route(
            "/workspaces/{id}/settings",
            get(settings_routes::get).put(settings_routes::put),
        )
        .route(
            "/workspaces/{id}/settings/test",
            post(settings_routes::test),
        )
        .route("/workspaces/{id}/members", get(members_routes::list))
        .route(
            "/workspaces/{id}/members/{user_id}",
            axum::routing::put(members_routes::set_role).delete(members_routes::remove),
        )
        .route("/users", get(members_routes::org_users))
        // Deactivated accounts: without this route reactivation is out of reach (the person
        // has vanished from every list)
        .route("/users/deactivated", get(members_routes::deactivated_users))
        .route(
            "/kbs/{id}",
            patch(kbs::update).get(kbs::get_one).delete(kbs::delete),
        )
        .route("/kbs/{id}/members", get(kbs::members))
        .route("/kbs/{id}/audit", get(kbs::audit_log))
        // Requeue failed jobs (#216): within a KB for Editor, globally for admins
        .route("/kbs/{id}/jobs/failed", get(jobs_routes::failed_in_kb))
        .route("/kbs/{id}/jobs/requeue", post(jobs_routes::requeue_in_kb))
        .route("/jobs/requeue", post(jobs_routes::requeue_all))
        .route(
            "/kbs/{id}/members/{user_id}",
            axum::routing::put(kbs::set_member).delete(kbs::remove_member),
        )
        .route(
            "/admin/deployment",
            get(admin_routes::get_deployment).put(admin_routes::put_deployment),
        )
        .route("/admin/users", post(admin_routes::create_user))
        // Deactivate / reactivate an account (see `users.deactivated_at`). DELETE means "this
        // person no longer has access", not "this row is gone" -- attribution stays queryable
        .route(
            "/admin/users/{id}",
            axum::routing::delete(admin_routes::deactivate_user)
                .post(admin_routes::reactivate_user),
        )
        .route(
            "/admin/data-sources",
            get(datasource_routes::list).post(datasource_routes::create),
        )
        .route(
            "/admin/data-sources/{id}",
            axum::routing::delete(datasource_routes::delete),
        )
        .route(
            "/admin/data-sources/{id}/test",
            post(datasource_routes::test),
        )
        .route(
            "/admin/data-sources/{id}/grants",
            get(datasource_routes::grants),
        )
        .route(
            "/admin/data-sources/{id}/grants/{workspace_id}",
            axum::routing::put(datasource_routes::grant).delete(datasource_routes::revoke),
        )
        .route("/kbs/{id}/mcp", post(mcp::handle))
        .route(
            "/me/tokens",
            get(token_routes::list).post(token_routes::issue),
        )
        .route(
            "/me/tokens/{token_id}",
            axum::routing::delete(token_routes::revoke),
        )
        .route("/kbs/{id}/mappings", get(mapping_routes::list))
        .route(
            "/kbs/{id}/mappings/{mapping_id}",
            axum::routing::patch(mapping_routes::revise),
        )
        .route(
            "/kbs/{id}/mappings/{mapping_id}/revisions",
            get(mapping_routes::revisions),
        )
        .route("/kbs/{id}/data-sources", get(datasource_routes::mounted))
        .route(
            "/kbs/{id}/data-sources/available",
            get(datasource_routes::mountable),
        )
        .route(
            "/kbs/{id}/data-sources/{ds_id}",
            axum::routing::put(datasource_routes::mount).delete(datasource_routes::unmount),
        )
        .route(
            "/kbs/{id}/data-sources/{ds_id}/sync-schema",
            post(datasource_routes::sync_schema),
        )
        .route(
            "/kbs/{id}/data-sources/explore",
            post(datasource_routes::explore),
        )
        .route(
            "/kbs/{id}/documents",
            get(documents_routes::list)
                .post(documents_routes::upload)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        // One-click retry: extraction failures usually come in batches (the model endpoint was
        // down for a while and everything that arrived in that window failed)
        .route(
            "/kbs/{id}/documents/retry-failed",
            post(documents_routes::retry_failed),
        )
        .route(
            "/kbs/{id}/extraction-drops",
            get(documents_routes::extraction_drops),
        )
        .route("/kbs/{id}/ontology", get(ontology_routes::get))
        .route(
            "/kbs/{id}/ontology/type-resolution/preview",
            post(ontology_routes::type_resolution_preview),
        )
        .route(
            "/kbs/{id}/ontology/type-resolution",
            post(ontology_routes::type_resolution_apply),
        )
        .route(
            "/kbs/{id}/ontology/type-resolution/approve",
            post(ontology_routes::approve_refinement),
        )
        .route(
            "/kbs/{id}/ontology/type-resolution/{batch_id}",
            axum::routing::delete(ontology_routes::type_resolution_undo),
        )
        .route(
            "/kbs/{id}/ontology/entity-types",
            post(ontology_routes::create_entity_type),
        )
        .route(
            "/kbs/{id}/ontology/entity-types/{type_id}",
            patch(ontology_routes::update_entity_type).delete(ontology_routes::delete_entity_type),
        )
        .route(
            "/kbs/{id}/ontology/entity-types/{type_id}/entities",
            get(ontology_routes::list_entity_instances),
        )
        .route(
            "/kbs/{id}/ontology/relation-types",
            post(ontology_routes::create_relation_type),
        )
        .route(
            "/kbs/{id}/ontology/relation-types/{type_id}",
            patch(ontology_routes::update_relation_type)
                .delete(ontology_routes::delete_relation_type),
        )
        .route(
            "/kbs/{id}/ontology/misses/dismiss",
            post(ontology_routes::dismiss_miss),
        )
        .route(
            "/kbs/{id}/ontology/misses/restore",
            post(ontology_routes::restore_miss),
        )
        .route("/kbs/{id}/ontology/suggest", post(ontology_routes::suggest))
        // The ones computed last time that nobody has ruled on yet (see `ontology_proposals`).
        // A page refresh leans on this instead of re-running the model
        .route(
            "/kbs/{id}/ontology/proposals",
            get(ontology_routes::stored_proposals).post(ontology_routes::decide_proposal),
        )
        // OWL import: preview and commit are two separate endpoints -- an upload must never
        // change the ontology by itself.
        // Both run the same plan -- separate code paths drift apart, and drift means what
        // happens after you confirm is not what you just looked at
        .route(
            "/kbs/{id}/ontology/imports",
            get(ontology_routes::list_imports)
                .post(ontology_routes::apply_import)
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/kbs/{id}/ontology/imports/preview",
            post(ontology_routes::preview_import).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/kbs/{id}/ontology/proposed-predicates",
            get(ontology_routes::proposed_predicates),
        )
        .route(
            "/kbs/{id}/ontology/auto-extension",
            get(ontology_routes::last_auto_extension),
        )
        .route(
            "/kbs/{id}/ontology/adopt-predicate",
            post(ontology_routes::adopt_predicate),
        )
        .route(
            "/kbs/{id}/ontology/adopt-predicate/{batch_id}",
            axum::routing::delete(ontology_routes::unadopt_predicate),
        )
        .route("/kbs/{id}/search", post(search_routes::search))
        .route("/kbs/{id}/chat", post(chat::chat))
        // Reattach to the answer still being generated after a page refresh (see `live`)
        .route(
            "/kbs/{id}/conversations/{conversation_id}/stream",
            get(chat::reattach),
        )
        .route("/kbs/{id}/conversations", get(chat::list_conversations))
        .route(
            "/kbs/{id}/conversations/{conversation_id}",
            get(chat::conversation_detail)
                .patch(chat::rename_conversation)
                .delete(chat::delete_conversation),
        )
        .route(
            "/documents/{id}",
            get(documents_routes::detail).delete(documents_routes::delete),
        )
        .route("/documents/{id}/extract", post(graph_routes::extract))
        .route("/kbs/{id}/graph/overview", get(graph_routes::overview))
        .route(
            "/kbs/{id}/graph/neighborhood",
            get(graph_routes::neighborhood),
        )
        .route("/kbs/{id}/entities", get(graph_routes::search_entities))
        .route(
            "/kbs/{id}/entities/{entity_id}",
            get(graph_routes::entity_detail).patch(graph_routes::update_entity),
        )
        .route(
            "/kbs/{id}/entities/{entity_id}/history",
            get(graph_routes::entity_history),
        )
        .route(
            "/kbs/{id}/facts/{fact_id}/evidence",
            get(graph_routes::fact_evidence),
        )
        // Proof of a derived fact (0002 R2): premises expanded in order down to the sentence
        .route(
            "/kbs/{id}/derived/{derived_id}/proof",
            get(graph_routes::derived_proof),
        )
        // Proof of a derivation that never landed (0017 §3): premises live in the violation's path
        .route(
            "/kbs/{id}/violations/{violation_id}/proof",
            get(graph_routes::blocked_proof),
        )
        .route("/kbs/{id}/events", get(events_routes::kb_events))
        .route(
            "/kbs/{id}/sources",
            get(sources_routes::list).post(sources_routes::create),
        )
        .route(
            "/kbs/{id}/sources/{source_id}",
            patch(sources_routes::update).delete(sources_routes::delete),
        )
        .route(
            "/kbs/{id}/sources/{source_id}/sync",
            post(sources_routes::sync_now),
        )
        .route(
            "/kbs/{id}/sources/{source_id}/runs",
            get(sources_routes::runs),
        )
        .route(
            "/kbs/{id}/sources/{source_id}/re-extract",
            post(sources_routes::re_extract),
        )
        .route("/kbs/{id}/graph/rebuild", post(graph_routes::rebuild))
        .route(
            "/kbs/{id}/sources/{source_id}/missing/cleanup",
            post(sources_routes::cleanup_missing),
        )
        .route(
            "/documents/{id}/extractions",
            get(documents_routes::extractions),
        )
        .route("/kbs/{id}/ingest", post(sources_routes::ingest))
        // Push from an api source: authenticated by a source-specific key (Bearer), no session
        .route("/sources/{source_id}/ingest", post(sources_routes::push))
        .route(
            "/kbs/{id}/sources/{source_id}/token",
            get(sources_routes::get_token),
        )
        .route(
            "/kbs/{id}/sources/{source_id}/rotate-token",
            post(sources_routes::rotate_token),
        )
        .route("/kbs/{id}/review", get(review_routes::list))
        .route("/kbs/{id}/review/history", get(review_routes::history))
        // Facts extracted from a memory, waiting for a nod (0015): fetched per sentence, ruled
        // on one at a time
        .route(
            "/kbs/{id}/review/pending",
            get(review_routes::pending_for_chunk),
        )
        .route(
            "/kbs/{id}/review/pending/{pending_id}",
            post(review_routes::decide_pending),
        )
        .route("/kbs/{id}/review/{review_id}", post(review_routes::decide))
        // Ruling on a semantic-layer mapping (0011). Sits alongside resolution review -- both
        // are "the engine proposes, a human decides"
        .route(
            "/kbs/{id}/review/mappings/{mapping_id}",
            post(review_routes::decide_mapping),
        )
        // Consistency check (0002 R0): run a pass, and rule on one violation.
        // The check itself is pure computation, so it runs synchronously
        .route(
            "/kbs/{id}/consistency/check",
            post(review_routes::run_consistency_check),
        )
        .route(
            "/kbs/{id}/review/violations/{violation_id}",
            post(review_routes::decide_violation),
        )
        .route(
            "/kbs/{id}/review/defects/{defect_id}",
            post(review_routes::decide_defect),
        )
        // R1 materialised inference. Bounded by the materialize_inferences switch on the KB
        .route(
            "/kbs/{id}/inference/run",
            post(review_routes::run_inference),
        )
        .route(
            "/kbs/{id}/facts/{fact_id}/confirm",
            post(review_routes::confirm_fact),
        )
        .route(
            "/kbs/{id}/facts/{fact_id}/reject",
            post(review_routes::reject_fact),
        )
        .route(
            "/kbs/{id}/facts/{fact_id}/close",
            post(review_routes::close_fact),
        )
        .route(
            "/kbs/{id}/merges/{merge_id}/revert",
            post(review_routes::revert_merge),
        )
        .route(
            "/kbs/{id}/conflicts/{conflict_id}",
            post(review_routes::resolve_conflict),
        )
        .route(
            "/kbs/{id}/entities/merge",
            post(review_routes::manual_merge),
        )
        .route(
            "/documents/{id}/reprocess",
            post(documents_routes::reprocess),
        )
        .route("/jobs/noop", post(jobs_noop))
        .with_state(state);

    // CORS for development: the Vite dev server sends cookies across ports
    let cors = CorsLayer::new()
        .allow_origin("http://localhost:5173".parse::<HeaderValue>().unwrap())
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::PUT,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        .allow_credentials(true);

    let mut app = Router::new().nest("/api/v1", api).layer(cors);

    // Serving the SPA: mount it if the build output exists, with a history fallback to index.html
    let index = std::path::Path::new(&cfg.web_dist).join("index.html");
    if index.exists() {
        let serve = ServeDir::new(&cfg.web_dist).fallback(ServeFile::new(index));
        app = app.fallback_service(serve);
        tracing::info!("serving the frontend build: {}", cfg.web_dist);
    }

    // Outermost layer: put the request's origin into a task-local first, so that any layer
    // writing an audit record afterwards can read it
    app.layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(crate::client_ctx::capture))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "name": "utopia", "version": env!("CARGO_PKG_VERSION") }))
}

/// P0 queue smoke-test endpoint: enqueues a noop job (removed in a later milestone).
async fn jobs_noop(
    axum::extract::State(state): axum::extract::State<AppState>,
    _user: crate::auth::AuthUser,
) -> crate::error::ApiResult<Json<serde_json::Value>> {
    let id = utopia_store::jobs::enqueue(&state.pool, "noop", json!({})).await?;
    Ok(Json(json!({ "job_id": id })))
}
