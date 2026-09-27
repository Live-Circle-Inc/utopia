-- Data source grants: which workspaces this database may be used by.
--
-- **Before this, this layer did not exist.** Registration is a deployment-level action
-- (`require_admin`), while the guard on mounting is `require_kb(kb_id, Role::Admin)` -- the
-- admin of the requester's own KB. And the mountable list returns `datasources::list(pool)`:
-- every single source in the deployment, unfiltered.
--
-- So the admin of any one knowledge base could list every registered database in the
-- deployment and mount any of them into their own KB. Once mounted, every Viewer of that KB
-- can run read-only SQL against it through `query_data` (asking about data is reading, and a
-- viewer may do it too). In a multi-workspace deployment this is cross-tenant.
--
-- **Why a new table rather than adding a `workspace_id` column to `data_sources`:**
-- that column would mean every data source belongs to exactly one workspace. When the company
-- has a single warehouse and several departments each with their own workspace, that warehouse
-- could only be used by one department -- degrading a relation that is inherently many-to-many
-- into a one-to-many. Granting is many-to-many by nature: one source can be granted to several
-- workspaces, and one workspace can receive several sources.
--
-- **The division of labour with `kb_data_sources`** (not one field of that table changes):
--
--   grant = the system admin says "which workspaces may use this database"  ← this table
--   mount = the KB admin says "which ones my KB mounts"                     ← kb_data_sources
--
-- Both layers are M2M, each with its own owner. Mounting can no longer happen out of thin
-- air: it can only pick from the set that was granted.
CREATE TABLE data_source_grants (
    data_source_id UUID NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    workspace_id   UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    granted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who granted it. **A bare foreign key**, consistent with entity_merges.merged_by and
    -- the others: users are soft-deleted, production code has no DELETE FROM users, so the
    -- attribution holds up
    granted_by     UUID REFERENCES users(id),
    PRIMARY KEY (data_source_id, workspace_id)
);

-- "which sources can this workspace use" is a hot path (asked once every time the data
-- mapping page is opened); the primary key already covers the reverse, "who was this source
-- granted to"
CREATE INDEX data_source_grants_workspace_idx ON data_source_grants (workspace_id);

-- Existing grants: backfill what is already mounted, otherwise the moment this migration
-- ships it cuts off every source currently in use.
--
-- **What is backfilled is the fait accompli, not a permissive default**: grant only to the
-- workspaces that "really have mounted it". Not a single one that never mounted it gets
-- anything -- that is exactly the door this migration is closing.
INSERT INTO data_source_grants (data_source_id, workspace_id)
SELECT DISTINCT kds.data_source_id, kb.workspace_id
  FROM kb_data_sources kds
  JOIN knowledge_bases kb ON kb.id = kds.kb_id
ON CONFLICT DO NOTHING;
