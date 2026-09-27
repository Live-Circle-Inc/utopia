//! utopia-store: sqlx repositories, migrations, job queue.
//! Everything uses runtime queries (not the compile-time macros), so the build needs no database.

pub mod access;
pub mod accounts;
pub mod alerts;
pub mod audit;
pub mod conversations;
pub mod datasources;
pub mod db;
pub mod documents;
pub mod extraction_drops;
pub mod graph;
pub mod jobs;
pub mod kbs;
pub mod mappings;
pub mod members;
pub mod memory;
pub mod model_limits;
pub mod ontology;
pub mod palette;
pub mod pending;
pub mod reasoning;
pub mod resolution;
pub mod review;
pub mod settings;
pub mod sources;
pub mod temporal;
pub mod test_db;
pub mod tokens;
pub mod workspaces;
