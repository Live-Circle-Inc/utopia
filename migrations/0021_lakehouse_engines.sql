-- Ask-the-Data engines widened to the HTTP protocol family: trino (Iceberg / Delta / Hive are all
-- catalogs of it), databricks (SQL Statement API), snowflake (SQL API v2).
-- The mount model has nothing to do with which engine is registered (0006's judgment still holds);
-- this only relaxes the values engine may take. The permitted names are the same list as
-- `query_engine::ENGINES`.
ALTER TABLE data_sources DROP CONSTRAINT data_sources_engine_check;
ALTER TABLE data_sources ADD CONSTRAINT data_sources_engine_check
    CHECK (engine IN ('postgres', 'trino', 'databricks', 'snowflake'));
