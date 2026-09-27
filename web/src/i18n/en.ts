// The English string pack. This one is **the authority on structure**: `Strings = typeof en`, and
// every other pack is shaped by it -- miss an entry and it will not compile (see
// docs/decisions/0004).
//
// Add new copy here first, then fill in the other packs -- do it the other way round and you get a
// type error, which is exactly the point.
export const en = {
  app: {
    name: "Utopia",
    // Adapted from the closing sentence of "Utopia" (Burnet's 1684 translation):
    // "there are many things in the commonwealth of Utopia that I rather wish,
    //  than hope, to see followed in our governments."
    // I became We, the parenthetical commas went, and the object of "to see" is left open.
    tagline: "We rather wish than hope to see.",
    taglineSource: "— Thomas More, 1516",
    siteUrl: "https://utopia.bi",
    docsUrl: "https://utopia.bi/docs",
  },
  /** Wording for server-side validation errors. key = the code the server sends; a missing one
      falls back to the English sentence instead of crashing. Contract guards (only reachable by
      calling the wrong endpoint) are deliberately not here -- their reader is a developer */
  err: {
    bad_email: "That doesn't look like an email address.",
    password_too_short: "Password must be at least 8 characters.",
    bad_display_name: "Display name must be 1-64 characters.",
    wrong_password: "Current password is incorrect.",
    registration_closed:
      "Sign-up is closed on this deployment — ask an administrator for an account.",
    no_chat_model:
      "No chat model configured yet. Set one under Settings → Models.",
    bad_upload: "That upload could not be read.",
    upload_read_failed: "The file could not be read to the end.",
    no_files: "No file was attached.",
    upload_needs_folder: "Files can only be uploaded into a folder source.",
    empty_file: "That file is empty.",
    file_too_large: "That file is too large — the limit is 8 MB.",
    bad_ontology_file: "That is not a readable OWL or RDFS file.",
    bad_name: "Name must be 1-64 characters.",
    default_kb_open:
      "The default knowledge base stays open to everyone — create a separate one for private work.",
    default_kb_undeletable: "The default knowledge base cannot be deleted.",
    last_owner_demote: "This is the last owner — promote someone else first.",
    last_owner_remove: "This is the last owner — hand ownership over first.",
    key_required: "A key is required.",
    forms_required: "Pick at least one phrase this relation covers.",
    bad_key:
      "Keys are lowercase, letters digits and underscores only, up to 40 characters.",
    self_parent: "A class cannot be its own parent.",
    parent_cycle:
      "That class is already below this one — the hierarchy would loop.",
    bad_lang: "Pick a supported language.",
    attr_needs_class: "An attribute has to belong to a class.",
    attr_has_no_link:
      "An attribute has no inverse and no super-property — its value is a literal, not something to point back from.",
    link_target_is_attr:
      "Pick a relation, not an attribute — an attribute's value is a literal.",
    sub_property_self: "A relation cannot be its own super-property.",
    unknown_relation: "That relation is not in this knowledge base.",
    entity_name_required: "Name cannot be empty.",
    entity_name_too_long: "Name is too long — 100 characters at most.",
    unknown_entity_type: "That class is not in this ontology.",
    nothing_to_update: "Nothing to change.",
    self_merge: "An entity cannot be merged into itself.",
    close_at_required:
      "Pick the date this fact ended — the new one does not say when it started.",
    empty_query: "Type something to search for.",
    no_data_sources: "No databases are mounted on this knowledge base.",
    // Grants are per workspace (0014): this source is not granted to the workspace this base is in
    source_not_granted:
      "This data source is not granted to this workspace. Ask a deployment admin to grant it in System settings → Data sources.",
    memory_source_permanent:
      "The Memory source is part of the knowledge base and stays.",
    source_name_required: "Give this source a name.",
    bad_cron: "That cron expression could not be parsed.",
    bad_cron_fields:
      "A cron expression has five fields: minute hour day month weekday.",
    ds_name_required: "Give this data source a name.",
    only_postgres: "Only PostgreSQL is supported for now.",
    bad_conn_string: "A connection string starts with postgres://",
    concurrency_range: "Pick a number between 1 and 256.",
    inference_off:
      "Materialized inference is off for this knowledge base. Turn it on in Settings.",
    bad_resolution: "That is not a valid decision.",
  },
  /** The machine's own detail (the cron parser's words, say) appended after the wording */
  errDetail: (msg: string, detail: string) => `${msg} (${detail})`,
  toast: {
    saved: "Saved",
    created: "Created",
    deleted: "Deleted",
    added: "Added to the ontology",
  },
  account: {
    /* Wordmark for the account area: Persona -- the mask you wear in this city */
    brand: "Utopia Persona",
    /* Short name used in the page title: `Utopia | Persona` */
    titleTag: "Persona",
    profile: "Profile",
    administration: "Administration",
    adminChip: "Admin",
    backToApp: "← Back to app",
    profileTitle: "Profile",
    displayName: "Display name",
    email: "Email",
    save: "Save",
    passwordTitle: "Change password",
    currentPassword: "Current password",
    newPassword: "New password (min. 8 characters)",
    changePassword: "Update password",
    passwordChanged: "Password updated",
    avatarHint: "Avatars are generated from your name for now.",
    language: "Language",
    kbsNav: "Knowledge bases",
    kbsTitle: "Knowledge bases",
    kbOpen: "Open",
    kbRestricted: "Restricted",
    kbStats: (docs: number, members: number) =>
      `${docs} doc${docs === 1 ? "" : "s"} · ${members} member${members === 1 ? "" : "s"}`,
    addedBy: (name: string, date: string) => `Added by ${name} · ${date}`,
    joinedOn: (date: string) => `Joined ${date}`,
    openToEveryone: "Open to everyone",
    deploymentAdmin: "Deployment admin",
    openKb: "Open",
    kbSettingsBtn: "Settings",
    /* Personal token page (0014 / 0016 A2): a key for an agent, acting as this person */
    tokensNav: "Agents & tokens",
    tokensTitle: "Personal access tokens",
    tokensHint:
      "A token lets an agent reach Utopia over MCP as you, never with more than you can do yourself. Read-only by default, limited to the bases you pick, revocable here at any time.",
    newToken: "New token",
    tokenName: "Name",
    tokenNamePlaceholder: "My laptop",
    tokenScope: "Scope",
    scopeRead: "Read",
    scopeWrite: "Write",
    scopeHint:
      "Effective rights are your own role ∩ this scope. Write is a ceiling, not a grant: a viewer's write token still cannot write.",
    tokenKbs: "Knowledge bases",
    kbsAllHint: "Nothing selected means every base you can open.",
    tokenExpires: "Expires",
    expiresDays: (n: number) => `${n} days`,
    expiresNever: "Never",
    issueToken: "Create token",
    issuedTitle: "Copy it now. It will not be shown again.",
    issuedHint:
      "Utopia keeps only a hash of it. If you lose it, revoke it here and create another.",
    copy: "Copy",
    copied: "Copied",
    mcpTitle: "MCP client configuration",
    mcpHint:
      "For Claude Code, Claude Desktop and other Streamable HTTP clients. Each base has its own endpoint, so pick the one the agent should talk to. Other clients spell the keys differently.",
    mcpBase: "Base",
    yourTokens: "Your tokens",
    noTokens: "No tokens yet.",
    allBases: "All bases",
    nBases: (n: number) => `${n} base${n === 1 ? "" : "s"}`,
    lastUsed: (d: string) => `last used ${d}`,
    neverUsed: "never used",
    expiresOn: (d: string) => `expires ${d}`,
    noExpiry: "no expiry",
    createdOn: (d: string) => `created ${d}`,
    revoke: "Revoke",
    revokeConfirm: "Revoke?",
    revokedOn: (d: string) => `revoked ${d}`,
    tokenDone: "Done",
    roleNames: {
      owner: "Owner",
      admin: "Admin",
      editor: "Editor",
      viewer: "Viewer",
    } as Record<string, string>,
  },
  docs: {
    /* Wordmark for the docs area: Charter -- the founding charter of the ideal city, in the same
       typeface and size as the main wordmark */
    brand: "Utopia Charter",
    backTitle: "Back to Utopia",
    searchPlaceholder: "Search the docs…",
    noResults: "No matches.",
  },
  // Alert wording lives on the client and is looked up by kind -- the server only sends kind and
  // detail, it produces no display copy (docs/decisions/0004)
  alerts: {
    title: "Alerts",
    badgeLabel: "Alerts",
    empty: "Nothing needs attention",
    emptyHint:
      "Ingestion, sync and model failures show up here instead of only in the logs.",
    markAllRead: "Mark all read",
    close: "Close",
    // Say what is being searched: the title wording lives on the client, the server cannot search
    // it, so don't let anyone think typing "sync failed" will return something
    searchPlaceholder: "Search sources, knowledge bases, errors",
    noMatch: "Nothing matches",
    andMore: (n: number) => `and ${n} more`,
    system: "System",
    // kind → one sentence saying what happened. The second says what to do -- that is what an
    // alert has over a log line. **One alert is one failure**, so there is no count in the title
    kinds: {
      "source.sync_failed": {
        title: "A source failed to sync",
        hint: "Nothing new came in from it. Check the source's settings.",
      },
      "data_source.schema_sync_failed": {
        title: "A data source is mounted, but its schema is not",
        hint: "Ask cannot see which tables exist, so it will guess column names. Check the connection, then use Refresh schema.",
      },
      "mapping.exploration_empty": {
        title: "Mapping exploration proposed nothing",
        hint: "The model read the mounted schema but found no metric or dimension to propose. Refresh the schema under Data mapping > Data sources, add column comments if you can, then run Explore again.",
      },
      "llm.unreachable": {
        title: "The model endpoint gave no usable answer",
        hint: "Extraction and embedding are stopped. Check the endpoint URL in system settings.",
      },
      "llm.rate_limited": {
        title: "The model endpoint is rate limiting us",
        hint: "Documents were retried and still turned away, so some are missing facts. Lower model concurrency in system settings, or raise the quota on the account.",
      },
      "llm.out_of_credit": {
        title: "The model account cannot pay for requests",
        hint: "Extraction and embedding are stopped and will not resume on their own. Top up the account, or point system settings at an endpoint that can serve.",
      },
    } as Record<string, { title: string; hint: string } | undefined>,
    // An unseen kind must still render: the frontend may lag behind a newly shipped alert source
    unknownKind: (kind: string) => kind,
    /** Pick the work back up once it is fixed (#216) */
    runAgain: "Run those again",
    requeued: (n: number) =>
      n === 1 ? "1 job back in the queue" : `${n} jobs back in the queue`,
  },

  kbScope: {
    deniedTitle: "You don't have access to this knowledge base",
    deniedBody:
      "The link points at a base you can't open. Ask whoever shared it to grant you access, or pick one of your own.",
    missingTitle: "This knowledge base is gone",
    missingBody:
      "It was deleted, or the link was mistyped. Your own bases are listed below.",
    myKbs: "My knowledge bases",
  },
  nav: {
    workspaceLabel: "Workspace",
    kbLabel: "Knowledge base",
    ask: "Chat",
    askHint: "Converse with your knowledge base — it can remember",
    search: "Search",
    searchHint: "Hybrid search",
    graph: "Graph",
    graphHint: "Entities & timelines",
    library: "Library",
    libraryHint: "Documents & ingestion",
    settings: "Settings",
    settingsHint: "Models & members",
    signOut: "Sign out",
    docs: "Docs",
    loading: "Loading…",
    serverUnreachable:
      "Punishment 500: Utopia has gone quiet — it isn't answering.",
    notFound: "Punishment 404: You are lost in Utopia.",
    returnHome: "Return home",
    reportIssue: "Report an issue",
    refresh: "Refresh",
  },
  login: {
    signIn: "Sign in",
    signUp: "Sign up",
    displayName: "Display name",
    email: "Email",
    password: "Password (min. 8 characters)",
    submitting: "One moment…",
    createAccount: "Create account",
    networkError: "Network error, please try again",
    // The customary consent phrasing: By continuing, you agree to the <Terms> and acknowledge
    // the <Privacy>.
    agreePrefix: "By continuing, you agree to the ",
    agreeAnd: " and acknowledge the ",
    agreeSuffix: ".",
    githubUrl: "https://github.com/deeplethe/utopia",
  },
  legal: {
    privacyTitle: "Privacy policy",
    termsTitle: "Terms of use",
    backToSignIn: "← Back to sign in",
    privacy: {
      title: "Privacy policy",
      note: "Default text bundled with Utopia. The organization operating this deployment may replace it with its own policy.",
      sections: [
        {
          h: "A self-hosted platform",
          body: [
            "Utopia runs entirely on infrastructure chosen by the organization that deployed it (the operator). The Utopia project has no access to this deployment: the software sends no telemetry, no analytics and no crash reports to anyone.",
          ],
        },
        {
          h: "What this instance stores",
          body: ["Everything below lives on the operator's own servers:"],
          bullets: [
            "Account details — your email address, display name and a hash of your password.",
            "Content — uploaded documents, text extracted from them, search indexes and embeddings, and the knowledge graph (entities, relations and their sources) built from that content.",
            "Activity — your conversations with the assistant, review decisions and ingestion logs, kept so the features that need them can work.",
          ],
        },
        {
          h: "Where data can leave this server",
          body: [
            "If the operator configures an external model provider for chat or embeddings, excerpts of documents and your messages are sent to that provider to answer questions and index content. Which provider — or whether a fully local model is used — is a deployment setting. Nothing else is sent anywhere.",
          ],
        },
        {
          h: "Who can see what",
          body: [
            "Access follows knowledge-base roles: viewers see the knowledge bases they were granted, editors can change content, admins manage members and settings. Deployment administrators can create accounts and see the user list.",
          ],
        },
        {
          h: "Retention and deletion",
          body: [
            "Deleting a document removes its stored content and index entries. Facts already extracted into the knowledge graph remain, with their provenance, until removed through Review. Deleting a knowledge base permanently removes its documents, graph and sources.",
          ],
        },
        {
          h: "Questions",
          body: [
            "This deployment is run by your organization. For questions about how your data is handled here, contact its administrator.",
          ],
        },
      ],
    },
    terms: {
      title: "Terms of use",
      note: "Default text bundled with Utopia. The organization operating this deployment may replace it with its own terms.",
      sections: [
        {
          h: "About these terms",
          body: [
            "This instance of Utopia is operated by the organization that deployed it, not by the Utopia project. Your use of it is governed by that organization's own policies; these default terms cover the basics until the operator replaces them.",
          ],
        },
        {
          h: "Your account",
          body: [
            "Keep your credentials to yourself. Administrators may create, suspend or remove accounts in line with the operator's policies.",
          ],
        },
        {
          h: "Acceptable use",
          body: [],
          bullets: [
            "Upload only content you are authorized to store and share within your organization.",
            "Respect access levels: do not attempt to view or change knowledge bases beyond the roles you were granted.",
            "Do not use the platform to store or spread unlawful content.",
          ],
        },
        {
          h: "AI-generated answers",
          body: [
            "Answers from the assistant are generated from your organization's documents by a language model, with citations. They can be wrong or incomplete — verify against the cited sources before relying on them.",
          ],
        },
        {
          h: "The software",
          body: [
            "Utopia is open-source software provided “as is”, without warranty of any kind. Responsibility for operating this deployment — including backups, availability and compliance — lies with the operator.",
          ],
        },
      ],
    },
  },
  library: {
    title: "Library",
    upload: "Upload files",
    uploading: "Uploading…",
    uploadFailed: "Upload failed",
    dropHint: "Drag files here, or click “Upload files”",
    formats:
      "PDF · Word · Excel · PowerPoint · Markdown · HTML · TXT · CSV and more",
    emptyPull: "No documents yet — they arrive when this source syncs.",
    filterPlaceholder: "Filter by name",
    filterNoMatch: "No documents match your filter.",
    anyStatus: "Any extraction state",
    statusFailed: "Failed",
    statusDone: "Extracted",
    statusQueued: "Queued",
    statusExtracting: "Extracting",
    statusNone: "Not extracted",
    retryFailed: (n: number) => `Retry ${n} failed`,
    retryQueued: (n: number) => `${n} queued for extraction`,
    colFile: "File",
    colStatus: "Status",
    colGraph: "Graph",
    colChunks: "Chunks",
    colSize: "Size",
    colSource: "Source",
    delete: "Delete",
    extract: "Extract",
    reExtract: "Re-extract",
    reprocess: "Reprocess",
    // Extraction progress (aggregated within the current view, refreshed by SSE)
    extractProgress: (done: number, total: number) =>
      `Extracting · ${done} / ${total}`,
    // Failure details: the chip opens up, no longer just a tooltip
    errorTitle: "Failure details",
    errorParse: "Ingestion pipeline",
    errorGraph: "Graph extraction",
    copyError: "Copy",
    errorCopied: "Error copied",
    /* Extraction drops: facts were extracted but never landed. This used to be completely silent
       -- something was missing from the graph and nobody could say what */
    dropsChip: (n: number) => `${n} dropped`,
    dropsTitle: "Facts that did not land",
    dropsNote:
      "These were extracted from the document but blocked on the way in. " +
      "Each line says why, and how many.",
    dropsExample: "e.g.",
    dropReason: {
      attr_domain_mismatch: "Attribute on the wrong class",
      subject_not_declared: "Subject type unknown",
      attr_no_value: "Attribute had no value",
      attr_datatype: "Value did not match the datatype",
      low_confidence: "Below the confidence threshold",
      object_missing: "Relation had no object",
      malformed_item: "The model's item did not fit the schema",
      truncated_reply: "The model's reply was cut off",
      domain_mismatch:
        "The subject does not fit the relation, and swapping would not help",
      not_an_entity_name: "That name is a sentence, not a thing",
      direction_corrected:
        "Subject and object were swapped to match the signature",
    } as Record<string, string>,
    // Source-level re-extraction: not dangerous, only slow and expensive -- a light confirm, with
    // copy that states the cost and what is kept outright
    reExtractSource: "Re-extract",
    reExtractTitle: "Re-extract this source?",
    reExtractHint: (n: number, name: string) =>
      `All ${n} ready document${n === 1 ? "" : "s"} in “${name}” go through the extraction model again. ` +
      `Existing merges, review decisions and confirmed facts are preserved.`,
    reExtractConfirm: "Re-extract",
    queuedDocs: (n: number) => `${n} document${n === 1 ? "" : "s"} queued`,
    // Rebuilding the whole base: destructive -- type-the-name confirmation
    rebuild: "Rebuild graph",
    rebuildTitle: "Rebuild the knowledge graph?",
    rebuildHint: (docs: number, name: string) =>
      `Type “${name}” to confirm. Every entity, fact, merge and pending review in this knowledge base ` +
      `is permanently removed, then all ${docs} document${docs === 1 ? "" : "s"} are re-extracted from scratch. ` +
      `Documents, search indexes and the ontology are untouched; the decision ledger is kept.`,
    rebuildConfirm: "Rebuild permanently",
    rebuildDone: (e: number, f: number, q: number) =>
      `Cleared ${e} entities and ${f} facts · ${q} documents queued`,
    status: {
      pending: "Queued",
      parsing: "Parsing",
      indexing: "Indexing",
      embedding: "Embedding",
      ready: "Ready",
      failed: "Failed",
    },
    graphStatus: {
      none: "—",
      queued: "Queued",
      extracting: "Extracting",
      done: "Done",
      failed: "Failed",
    },
    sources: "Sources",
    allDocs: "All documents",
    uploads: "Uploads",
    addSource: "Add source",
    sourceKinds: {
      folder: "Folder",
      url: "URLs",
      rss: "RSS feed",
      api: "API",
      custom: "Custom",
      github_issues: "GitHub issues",
      jira_issues: "Jira issues",
      s3: "S3 / MinIO",
      azure_blob: "Azure Blob",
      gcs: "Google Cloud Storage",
      webdav: "WebDAV",
      notion: "Notion",
    },
    sourceKindHints: {
      folder:
        "A plain folder. Select it and upload (or drag) files straight into it — nothing is watched or synced.",
      url: "Fetches the listed web pages; changed pages update the same document.",
      rss: "Subscribes to a feed; each entry becomes a document dated by its publish time.",
      jira_issues:
        "Syncs a Jira project's issues. **Each ticket, together with its field-level change " +
        "history**, becomes one document — what changed from what to what, and when. " +
        "One call fetches everything; no per-ticket round trips.",
      github_issues:
        "Syncs a repository's issues. **Each ticket, together with its state history**, becomes " +
        "one document — when it was opened, closed, relabelled, reassigned, all dated. " +
        "Without a token GitHub allows only 60 requests an hour.",
      s3:
        "Reads documents out of an S3 bucket, or anything speaking the same protocol " +
        "(MinIO, Ceph, R2). **Leave the endpoint empty for AWS**; fill it in for a " +
        "self-hosted one. Each object becomes a document dated by its last modification.",
      azure_blob:
        "Reads documents out of an Azure Blob container. Leave the endpoint empty for " +
        "Azure itself; fill it in for Azurite or a gateway.",
      gcs:
        "Reads documents out of a Google Cloud Storage bucket. The service account JSON " +
        "goes in whole — there is no file on the server to point at.",
      webdav:
        "Reads documents off a WebDAV share — Nextcloud, ownCloud, Synology, or anything " +
        "else speaking the protocol. Walks folders one level at a time; each file becomes " +
        "a document dated by its last modification.",
      notion:
        "Syncs the pages a Notion integration can see — share a page with the integration " +
        "and it appears here. Dated by when the page was last edited, which is the page's " +
        "own clock rather than ours.",
      api: "External systems push JSON documents here, authenticated with this source's own token.",
      custom:
        "Polls a URL you control on a schedule — your service returns JSON items and Utopia keeps them in sync.",
      memory:
        "Episodes remembered from Chat. Append-only: contradicted memories close their " +
        "validity range instead of being deleted — the timeline keeps the whole story.",
    },
    ingestGuide: "Read the ingest guide →",
    ingestGuideTitle: "Ingest interface guide",
    endpointField: "Endpoint URL",
    endpointCopied: "Endpoint URL copied",
    copyEndpoint: "Click to copy the full URL",
    // Push status of api sources (queued/running never occur; they are here for type completeness)
    pushStatus: {
      never: "No pushes yet",
      queued: "—",
      running: "—",
      ok: "Push received",
      failed: "Push failed",
    },
    tokenTitle: "Source token",
    tokenWarning:
      "Anyone holding this token can push documents into this source. Rotate it if it leaks — the old token stops working immediately.",
    tokenUsage: "Send it with every push as:",
    tokenCopied: "Token copied",
    viewToken: "Token",
    rotateToken: "Rotate",
    noToken: "This source has no token yet — generate one to start pushing.",
    generateToken: "Generate token",
    close: "Close",
    authHeaderField: "Authorization header (optional, never shown again)",
    syncNow: "Sync now",
    syncStatus: {
      never: "Never synced",
      queued: "Queued",
      running: "Syncing…",
      ok: "Synced",
      failed: "Sync failed",
    },
    lastSyncAdded: (n: number) => `+${n} last sync`,
    sourceName: "Name",
    urlsField: "Page URLs (one per line)",
    feedUrl: "Feed URL",
    repoField: "Repository (owner/name)",
    jiraUrlField: "Jira site URL",
    jiraProjectField: "Project key",
    s3BucketField: "Bucket",
    s3PrefixField: "Prefix (optional — without one the whole bucket is read)",
    s3EndpointField: "Endpoint (leave empty for AWS S3)",
    s3RegionField: "Region",
    s3KeyField: "Access key ID",
    s3SecretField: "Secret access key",
    azAccountField: "Storage account name",
    azKeyField: "Account key",
    gcsKeyField: "Service account JSON (paste the whole file)",
    davUrlField: "Server URL",
    davPathField: "Folder (optional — defaults to the root)",
    davUserField: "Username",
    davPassField: "Password",
    notionTokenField: "Internal integration token",
    notionQueryField: "Search term (optional — empty takes every shared page)",
    tokenField: "GitHub token (optional, never shown again)",
    includePullRequests: "Treat pull requests as tickets too",
    interval: "Sync schedule",
    intervalManual: "Manual only",
    intervalEvery: (m: number) =>
      m < 60 ? `Every ${m} min` : `Every ${m / 60} h`,
    schedule: {
      manual: "Manual",
      interval: "Interval",
      daily: "Daily",
      weekly: "Weekly",
      advanced: "Advanced",
      every: "Every",
      minutes: "minutes",
      hours: "hours",
      at: "at",
      cronPlaceholder: "Enter a cron expression",
      whatIsCron: "What is cron?",
      cronDocsUrl: "https://crontab.guru",
      daysShort: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
      dailyAt: (t: string) => `Daily ${t}`,
    },
    createSource: "Create",
    cancel: "Cancel",
    deleteSource: "Delete source",
    deleteSourceHint: "Documents are kept and move to Uploads.",
    newSourceTitle: "New source",
    iconLabel: "Icon",
    pageOf: (from: number, to: number, total: number) =>
      `${from}–${to} of ${total}`,
    syncHistory: "History",
    runNew: (n: number) => `+${n} new`,
    runUpdated: (n: number) => `~${n} updated`,
    runNothing: "no changes",
    noRuns: "No syncs recorded yet.",
    sourceSettings: "Source settings",
    editSourceTitle: "Source settings",
    saveChanges: "Save changes",
    authHeaderEditField: "Authorization header",
    authKeepHint: "Leave blank to keep the current value.",
    editKeepNote:
      "Changing what this source fetches never deletes existing documents. " +
      "Items the new configuration no longer returns are marked “Not in source” " +
      "after the next sync. Switching to a different service entirely? Create a new source instead.",
    notInSource: "Not in source",
    cleanupMissing: (n: number) => `Clean up ${n} missing`,
    cleanupTitle: "Delete missing documents",
    cleanupHint: (n: number, name: string) =>
      `${n} document${n === 1 ? "" : "s"} in “${name}” ${n === 1 ? "is" : "are"} no longer ` +
      "present in the source. Deleting removes their content and search entries permanently. " +
      "Facts already extracted into the graph remain, with their provenance.",
    cleanupConfirm: "Delete them",
    deleteSourceTitle: "Delete this source",
    deleteSourceBody: (name: string) =>
      `Type “${name}” to confirm. Documents are kept and move to Uploads; ` +
      "scheduled syncing stops.",
    dangerZone: "Danger zone",
  },
  search: {
    placeholder: "Search the knowledge base… (keyword + semantic)",
    button: "Search",
    searching: "Searching…",
    noResults: "No results found",
    chunkOf: (filename: string, seq: number) => `${filename} · section ${seq}`,
  },
  ask: {
    /* Greeting on a new conversation's first screen: the inscriptional serif, with the brand name
       inside the sentence (a heading takes no full stop) */
    greeting: "Ask Utopia what it remembers",
    emptyTitle: "Chat",
    emptyBody:
      "Converse with your knowledge base — cited answers, temporal questions, and it can remember.\nUpload documents in Library and configure a model in Settings first.",
    placeholder: "Ask anything…",
    composerHint: "Enter to send · Shift+Enter for a new line",
    scopeLabel: "Knowledge base",
    send: "Send",
    stop: "Stop",
    thinking: "Thinking…",
    newChat: "New chat",
    untitled: "Untitled",
    noConversations: "No conversations yet.",
    deleteConversation: "Delete conversation",
    searchConversations: "Search chats",
    moreActions: "More",
    rename: "Rename",
    copyTitle: "Copy title",
    deleteTitle: "Delete conversation?",
    deleteHint: (name: string) =>
      `“${name}” and its messages will be permanently removed.`,
    deleteBtn: "Delete",
    cancel: "Cancel",
  },
  graph: {
    // Entities with no type decided yet (0009). Not a class, but "this slot is still empty"
    untyped: "Untyped",
    legendMore: (n: number) => `All ${n} classes`,
    nodeBudget: "How many entities to draw",
    nodeBudgetMore: "Draw more",
    nodeBudgetLess: "Draw fewer",
    legendSearch: "Filter classes",
    legendNone: "No class matches",
    legendOnly: "Only",
    legendShowAll: (n: number) => `Show all (${n} hidden)`,
    legendAllHint:
      "Every class on screen, most common first. Click to show or hide.",
    searchMore: (n: number) => `${n} more — load 20`,
    zoomIn: "Zoom in",
    zoomOut: "Zoom out",
    fitView: "Fit view",
    layoutForce: "Force layout",
    layoutCircular: "Circular layout",
    layoutPack: "Cluster by type",
    searchEntity: "Search entities…",
    searchInSubgraph: "Search in subgraph…",
    backToOverview: "← Full graph",
    // The order here is not arbitrary: until a model is configured, uploaded documents just sit in
    // the queue and not one entity comes out. Configure the model first, then upload documents
    emptyBody:
      "The graph is empty. Configure a chat model in Settings first, then upload documents in the Library — entities and relations are extracted automatically.",
    facts: "facts",
    noFacts: "No facts for this entity yet",
    confidence: "confidence",
    evidence: "evidence",
    noEvidence: "No evidence recorded",
    noQuote: "(no quote)",
    /* The predicate the extractor read out of the text, normalised into an identifier. A wording
       outside the vocabulary is downgraded to related to, and the original meaning stays alive
       only here.
       **The wording must not claim this is a quotation**: a relation key can only be [a-z0-9_],
       so in a Chinese corpus the word for "purchased" comes out as purchases -- saying "the text
       said purchases" would be false.
       The verbatim sentence is right beside it in the evidence quote, nothing is lost. */
    proposedPredicate: (p: string) => `read from the text as “${p}”`,
    inferredPredicate:
      "not a relation in the ontology, this is the source's wording",
    unknownPredicate: "no relation stated",
    sectionRef: (filename: string, seq: number) =>
      `${filename} · section ${seq} →`,
    fromVersion: (v: number) => `v${v}`,
    staleEvidenceHint:
      "This evidence comes from an earlier version of the document. " +
      "The document has since been updated; the fact itself is unaffected.",
    staleFactChip: "unconfirmed",
    staleFactHint:
      "All evidence for this fact comes from earlier versions of its source documents — " +
      "the current content no longer states it. It may still be true; review it under Review.",
    /* Entity corrections: extraction gives a first call, a human may overturn it */
    edit: "Edit",
    editName: "Name",
    editType: "Type",
    editSave: "Save",
    editCancel: "Cancel",
    editSaved: "Entity updated",
    editEmptyName: "Name cannot be empty",
    /* Sharing a name is not an error -- two people called Zhang Wei can coexist. Hint, don't block */
    sameNameNote: (n: number) =>
      n === 1
        ? "One other entity shares this name."
        : `${n} other entities share this name.`,
    sameNameHint: "If they are the same thing, merge them under Review.",
    mergeInto: "Merge in",
    mergeIntoHint:
      "Fold that entity into this one. Its facts move here; merges can be reverted.",
    mergeConfirm: (from: string, into: string) =>
      `Merge “${from}” into “${into}”? Its facts move here. You can revert this from Review.`,
    viewRelations: "Relations",
    viewTimeline: "Timeline",
    /* The third view: the record timeline -- not "when did it happen" but "when did we come to
       think so" */
    viewHistory: "History",
    viewDerived: "Derived",
    derivedEdges: (n: number) => `${n} derived`,
    derivedHint:
      "Edges no one asserted — the engine worked them out from axioms your ontology declares. Each one shows the premises it came from.",
    derivedNoProof: "The premises are gone.",
    /* Disputes (0017 §3) */
    contestedChip: "disputed",
    contestedHint: (kind: string, derived: string | null) =>
      kind === "derived_contradiction"
        ? `A derivation contradicts this assertion${derived ? `: ${derived}` : ""}. Open it under Review.`
        : kind === "temporal_conflict"
          ? "A newer assertion conflicts with this one in time. Open it under Review."
          : "This assertion breaks an axiom the ontology declares. Open it under Review.",
    blockedTitle: "Did not land",
    blockedHint:
      "The engine could draw these edges; an assertion stood in the way. They show on the graph as ghost edges.",
    blockedBy: (t: string) => `blocked by ${t}`,
    blockedReview: "Review",
    /** Proof chain (0002 R2): every step is an asserted premise, expanded down to the sentence */
    proofStep: (n: number) => `Step ${n}`,
    proofRetracted: "since retracted",
    proofLoading: "Tracing the proof…",
    derivedPanel: "Inference",
    derivedRunAsk: "Re-run inference for the whole base?",
    derivedRunGo: "Run",
    derivedRunCancel: "Cancel",
    derivedCountLabel: "Edges derived",
    derivedStateLabel: "Schedule",
    derivedLastLabel: "Last run",
    derivedOn: (mins: number) => `every ${mins} min`,
    derivedOff: "off",
    derivedNever: "never",
    derivedAgo: (mins: number) =>
      mins < 1
        ? "just now"
        : mins < 60
          ? `${mins} min ago`
          : `${Math.round(mins / 60)} h ago`,
    derivedRun: "Run now",
    derivedRunning: "Running…",
    derivedNoChange: "Nothing changed.",
    derivedChanged: (added: number, gone: number) =>
      `${added} added · ${gone} retracted`,
    derivedCapped: (n: number) => `${n} predicate(s) not closed fully`,
    close: "Close",
    // Which rule a derived edge came from. **All four have to be here** -- a miss falls back to
    // the raw kind string, and that means nothing to the person reading it
    ruleNames: {
      transitive: "transitive",
      symmetric: "symmetric",
      inverse: "inverse",
      sub_property: "sub-property",
    } as Record<string, string | undefined>,
    historyHint: "How this entity's record changed — and who changed it.",
    historyEmpty: "Nothing recorded for this entity yet.",
    historyKind: {
      asserted: "Recorded",
      corrected: "Interval corrected",
      rejected: "Withdrawn",
      /* Folded into another assertion: not a word of content lost, not a withdrawal */
      merged: "Merged into an existing fact",
      /* What changed is the class on the node, not one single fact */
      retyped: "Type changed",
      retype_reverted: "Type change undone",
    } as Record<string, string>,
    historyEngine: "engine",
    /* Changes to the validity interval: after a correction the interval closes at some point */
    historyClosedAt: (t: string) => `closed at ${t}`,
    historyFrom: (t: string) => `from ${t}`,
    historyOngoing: "open-ended",
    historicalNote: (n: number) =>
      `${n} past fact${n === 1 ? "" : "s"} not shown — see Timeline →`,
    undated: "Undated",
    timelineEmpty: "No dated facts yet.",
    lastConfirmed: (d: string) => `confirmed ${d}`,
    correctedHint:
      "This interval was closed by reconciliation (automatic succession or a review decision), " +
      "not stated verbatim in a document. The superseded assertion remains in the ledger.",
    ongoing: "now",
    /* Must be visibly different from ongoing: conflating the two is exactly what migration 0046
       set out to fix -- the text said "former CEO" and the UI showed now */
    endedUnknown: "ended, date unknown",
    stats: (n: number, e: number, active: number | null) =>
      `${n} entities · ${e} facts${active === null ? "" : ` · ${active} active`}`,
    /** The canvas only draws the batch with the highest degree. **Say how many are drawn and how
     *  many there are in all** -- this used to print the cap, so a base with tens of thousands of
     *  entities read 150 in the top right corner forever */
    statsCapped: (
      shown: number,
      total: number,
      shownE: number,
      totalE: number,
      active: number | null,
    ) =>
      `showing ${shown} of ${total} entities · ${shownE} of ${totalE} facts${active === null ? "" : ` · ${active} active`}`,
    cappedHint: (shown: number, total: number) =>
      `The canvas draws the ${shown} best-connected entities of ${total}. Search to reach the rest.`,
    stabilizing: "Stabilizing layout",
    scrubUnitHint: "Step size for playback and for each bar",
    scrubUnitYear: "Yr",
    scrubUnitMonth: "Mo",
    scrubUnitDay: "Dy",
    scrubBarMerged: (n: number) => `each bar covers ${n} steps`,
    allTime: "All time",
    nowBtn: "Now",
    play: "Play timeline",
    pause: "Pause",
  },
  doc: {
    backToLibrary: "← Back to Library",
    sections: "sections",
    section: "Section",
    citedHere: "← cited here",
    loading: "Loading…",
    extracted: "Extracted",
    ongoing: "now",
  },
  settings: {
    title: "System settings",
    tabModels: "Models",
    tabMembers: "Users",
    tabKbs: "Knowledge bases",
    tabDeployment: "Deployment",
    newUser: "Create user",
    initialPassword: "Initial password (min. 8 characters)",
    createUserBtn: "Create",
    searchUsers: "Filter by name or email…",
    deployment: {
      openReg: "Allow self-registration",
      openRegHint:
        "When off, the sign-up form is closed and only admins can create accounts here.",
      workers: "Background workers",
      workersHint:
        "An outer ceiling on how many jobs run at once (1–256), there to stop work piling up " +
        "without bound. The real throttle is the per-model limit below, so keep this comfortably " +
        "above the sum of those. Takes effect immediately.",
      workersApply: "Apply",
      /* The real throttle: the constraint is the provider's rate limit, and that is per model */
      modelConcurrency: "Model concurrency",
      modelConcurrencyHint:
        "How many calls a model will take at once. The limit that matters belongs to the " +
        "provider and is per model — a local Ollama may manage two, a hosted API fifty. " +
        "Background work (extraction, resolution, indexing) waits for a slot; chat and search " +
        "never do. Takes effect immediately.",
      /* Deployment-level default, used when a base is created. Deliberately not named "system
         language" */
      ontologyLang: "Default ontology language",
      ontologyLangHint:
        "The language new knowledge bases start their ontology in — class descriptions go " +
        "into the extraction prompt, so this follows the documents you expect, not the " +
        "interface. Each knowledge base can change its own afterwards. " +
        "Interface language is a per-reader choice in the account menu.",
      modelDefault: "Default",
      modelReset: "Reset",
      modelResetHint:
        "Drop this model's own limit and fall back to the default.",
    },
    datasources: {
      tab: "Data sources",
      title: "Data sources",
      hint:
        "Read-only database connections for asking questions about your data in Chat. " +
        "Register connections here; each knowledge base mounts the ones it may query.",
      name: "Name",
      connString: "Connection string — the scheme picks the engine",
      // One line per form; the token goes in the password slot, and the Databricks path is the
      // httpPath from the console
      connSchemes:
        "postgres://user:pass@host:5432/db\n" +
        "trino://user[:pass]@host:8080/catalog[/schema]   (Iceberg, Delta Lake, Hive)\n" +
        "databricks://:TOKEN@host/sql/1.0/warehouses/ID?catalog=main\n" +
        "snowflake://:TOKEN@account.snowflakecomputing.com/DB/SCHEMA?warehouse=WH",
      add: "Add data source",
      test: "Test",
      testOk: "Connected",
      testFail: "Failed",
      neverTested: "Untested",
      remove: "Remove",
      grants: "Available to",
      grantsHint:
        "Which workspaces may use this source. **Once granted, KB admins in those workspaces choose whether to mount it** — " +
        "this controls what they can reach, not what they have mounted.",
      grantsNone:
        "Not granted to any workspace — no knowledge base can mount it.",
      grantAdd: "Grant a workspace…",
      grantRevoke: "Revoke",
      grantRevoked: (n: number) =>
        n === 0
          ? "Revoked."
          : `Revoked, and unmounted it from ${n} knowledge base(s).`,
    },
    kbs: {
      hint:
        "Every knowledge base in this deployment. Open ones are readable by all members; " +
        "restricted ones are invite-only. Creating a knowledge base is an admin action — " +
        "members switch between them from the top bar.",
      defaultChip: "Default",
      newKb: "New knowledge base",
      packsLabel: "Bundled ontologies",
      packsHint:
        "Optional. Packs declare direction, so subject and object cannot come out reversed. More can be imported later.",
      packsNone: "None — start from the ten seed relations",
      packsCount: (c: number, p: number) => `${c} classes · ${p} properties`,
      name: "Name",
      description: "Description",
      visibility: "Visibility",
      visOpen: "Open — everyone in this deployment",
      visRestricted: "Invited only",
      create: "Create",
      openSettings: "Settings",
      docs: (n: number) => `${n} docs`,
    },
    modelsIntro:
      "OpenAI-compatible protocol — DeepSeek, Qwen, GLM, Ollama, vLLM all work. Fully on-prem friendly.",
    chatModel: "Chat model",
    embedModel: "Embedding model (optional, enables semantic search)",
    baseUrl: "Base URL",
    model: "Model",
    apiKey: "API key",
    keyConfigured: "(configured — leave blank to keep)",
    save: "Save",
    saving: "Saving…",
    saved: "Saved",
    test: "Test connection",
    testing: "Testing…",
    chatLabel: "Chat",
    embedLabel: "Embedding",
    ok: (reply: string) => `Connected (${reply})`,
    okDim: (dim: number) => `Connected (dim ${dim})`,
  },
  ontology: {
    title: "Ontology",
    hint: "Classes & properties",
    tabClasses: "Classes",
    tabProperties: "Properties",
    newClass: "New class",
    newSubClass: "+ Sub-class",
    newProperty: "New property",
    filter: "Filter…",
    missesShort: "Unmatched",
    refineShort: "Refine types",
    refineTitle: "Refine types",
    refineHint:
      "Entities whose class is roughly right but not the most specific one available. Look first, then apply — retyping does not appear on any timeline, so this is the only place you get to see it before it happens.",
    refinePreview: "Look first",
    refineLooking: "Looking…",
    refineRun: "Run and apply",
    refineRunning: "Running…",
    refineNothing: "Nothing to refine.",
    refineCandidates: (n: number) => `${n} entities would be considered`,
    refineNoCandidates: "Retrieval found no class for this one.",
    refineModelSays: (t: string) => `the model called it “${t}”`,
    refineRetyped: (n: number) => `${n} retyped automatically`,
    refineUndo: "Undo this batch",
    refineUndone: (n: number) => `${n} put back`,
    refineForReview: (n: number) => `${n} need your call`,
    refineCrossesAxis: "different axis",
    refineApprovePair: "Approve this class pair",
    refineLeftAlone: (n: number) => `${n} left alone`,
    refineTopCandidate: (c: string) => `closest class was ${c}`,
    instances: "Instances",
    instanceFacts: (n: number) => `${n} facts`,
    description: "Description",
    descriptionHint:
      "Guides the extractor: what belongs here, with a couple of examples. Fed straight into the extraction prompt.",
    overviewHint:
      "The schema your extractor follows. Select a class or property on the left to edit it, or add new ones with the + buttons.",
    overviewStats: (c: number, p: number) => `${c} classes · ${p} properties`,
    attributes: "Attributes",
    attributesHint:
      "Literal-valued fields of this class (a person's salary, a contract's amount). Extracted with evidence and history, like any fact.",
    newAttribute: "New attribute",
    attrDatatype: "Value type",
    attrUnit: "Unit",
    attrUnitHint: "optional — e.g. CNY, %",
    attrSingle: "Single-valued — a new value closes the previous one",
    datatypeNames: {
      text: "Text",
      number: "Number",
      date: "Date",
      bool: "Yes / no",
    } as Record<string, string>,
    cancel: "Cancel",
    key: "Key",
    label: "Label",
    color: "Color",
    shapeColor: "Shape & color",
    parent: "Parent class",
    noParent: "(top level)",
    disjoint: "Cannot also be",
    disjointHint:
      "Classes nothing can belong to at the same time. A Person is not an Organisation. The consistency check uses this to find classes that can never have an instance.",
    noDisjoint: "No class excluded",
    disjointWithParent:
      "This class inherits from a class it says it cannot be — nothing could ever satisfy it.",
    /* With several parents the left column can draw only one spot; say which branch it hangs under */
    primaryParentHint: "Shown in the tree under the first one.",
    /* Type signature. The wording has to make clear it guides rather than gates -- when the
       ontology is wrong the model can still override it */
    signature: "Type signature",
    signatureHint:
      "Which classes this relation connects. It goes into the extraction prompt as a hint, " +
      "not a gate: it steers the model as it writes, and the text still wins when the " +
      "ontology is wrong.",
    domainLabel: "Subject",
    rangeLabel: "Object",
    anyType: "Any type",
    searchTypes: "Search classes…",
    temporal: "Temporal semantics",
    temporalState: "State (has interval)",
    temporalEvent: "Event (point in time)",
    temporalEternal: "Eternal (timeless)",
    functional: "Functional (single value at a time)",
    inverseFunctional: "Inverse functional (one subject per object at a time)",
    axioms: "Axioms",
    axiomsHint:
      "What this relation guarantees. These are not descriptions — they change what the system does: the temporal engine closes old values, and the reasoning engine adds edges to the graph.",
    functionalHint:
      "One subject, one value at a time. A new value closes the old one.",
    inverseFunctionalHint: "One object, one subject. A project has one lead.",
    transitive: "Transitive",
    transitiveHint: "A→B and B→C means A→C. The engine will add those edges.",
    symmetric: "Symmetric",
    symmetricHint: "A→B means B→A. The engine will add the other direction.",
    asymmetric: "Asymmetric",
    asymmetricHint:
      "A→B rules out B→A. Both directions get reported as a contradiction.",
    irreflexive: "Irreflexive",
    irreflexiveHint: "Nothing can point at itself through this relation.",
    axiomConflict:
      "Symmetric and asymmetric together hold only for a relation with no facts at all — one of the two is wrong.",
    noLink: "None",
    inverseOf: "Inverse",
    inverseOfHint:
      "The relation that says the same thing the other way round. Declare it on one side only — the other direction follows.",
    subPropertyOf: "Super-property",
    subPropertyOfHint:
      "The broader relation this one is a special case of. Stating the specific one also states the broader one.",
    linkMeansInverse: (p: string, q: string) =>
      `A ${p} B also means B ${q} A.`,
    linkMeansSuper: (p: string, q: string) => `A ${p} B also means A ${q} B.`,
    usage: (n: number) => `${n} in use`,
    builtin: "built-in",
    save: "Save",
    delete: "Delete",
    deleteBlocked: "In use — cannot delete",
    /* ---- OWL / RDFS import ---- */
    importShort: "Import",
    importTitle: "Import an ontology",
    importHint:
      "Load an OWL or RDFS file (.owl, .rdf, .ttl). Classes and properties are matched by IRI, so re-importing a newer version of the same vocabulary updates what it already created instead of duplicating it.",
    importPick: "Choose file",
    importChange: "Choose another",
    importReading: "Reading…",
    importApplying: "Importing…",
    importApply: "Import",
    importCancel: "Cancel",
    importParsed: (fmt: string, triples: number) =>
      `${fmt === "rdfxml" ? "RDF/XML" : "Turtle"} · ${triples.toLocaleString()} triples`,
    importNothing:
      "Nothing to import — no classes or properties found in this file.",
    /* Three columns in the plan: new / updated / key already taken */
    importWillCreate: (n: number) => `${n} new`,
    importWillUpdate: (n: number) => `${n} updated`,
    importKeyTaken: (n: number) => `${n} skipped`,
    importClasses: "Classes",
    importRelations: "Relations",
    importAttributes: "Attributes",
    /* Attributes cannot be created yet: they need a domain, and a domain waits for the classes
       to be created and their IRIs resolved */
    importAttributesLater:
      "Parsed, but not created yet — attributes need a class to hang from, which lands in the next step.",
    /* The first thing the preview has to warn about: functional makes the temporal engine close
       old facts automatically. That one wrong uniqueness claim on part_of manufactured 59 false
       conflicts */
    warnFunctional: (n: number) =>
      `${n} ${n === 1 ? "relation declares" : "relations declare"} itself functional`,
    warnFunctionalBody:
      "A functional relation may hold one value at a time, so a new fact automatically closes the previous one. When the vocabulary claims uniqueness your data does not keep, that shows up as a queue of conflicts. Review these after importing.",
    /* The second thing: a description goes verbatim into the extraction prompt, and a class
       without one extracts noticeably worse */
    warnNoDescription: (n: number) =>
      `${n} ${n === 1 ? "class arrives" : "classes arrive"} with no description`,
    warnNoDescriptionBody:
      "A class description goes verbatim into the extraction prompt — it is the only thing telling the model what belongs there. Write one for these, or they will quietly under-extract.",
    /* Key collision: report it, don't resolve it. Auto-suffixing would leave the next re-import
       unable to recognise which entry it created last time */
    warnKeyTaken: (n: number) =>
      `${n} ${n === 1 ? "key is" : "keys are"} already taken`,
    warnKeyTakenBody:
      "Something else already holds this key under a different identity. These are left alone — rename the existing one first if you want the imported version instead.",
    /* An occupant with no IRI = built by hand in this base, or built in; saying that is more use
       than an empty IRI */
    importTakenBy: (iri: string | null) =>
      iri
        ? `taken by ${iri}`
        : "taken by an entry defined in this knowledge base",
    /* Axioms that appeared but are not projected today, listed by name and count -- "not
       projected yet" is not "skipped" */
    importUnprojected: "Not projected yet",
    importUnprojectedBody:
      "Axioms this file uses that Utopia does not consume yet. Nothing is lost: the source file is stored as uploaded, so a later version can project them.",
    importDone: (created: number, updated: number) =>
      `Imported — ${created} classes created, ${updated} updated.`,
    importHistory: "Previous imports",
    importNoHistory: "No imports yet.",
    importBy: (who: string, when: string) => `${who} · ${when}`,
    importSize: (bytes: number) =>
      bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(0)} KB`,
    misses: "Unmatched from extraction",
    missesHint:
      "The extractor produced these outside your ontology (they fell back to concept / related to). They are signals for extending the ontology.",
    dismiss: "Dismiss",
    dismissed: (n: number) => `Dismissed (${n})`,
    /* The number is the one that **kept growing after it was dismissed** -- that is the entire
       point of this line: the "it only showed up once" that justified the call may have stopped
       being true long ago */
    dismissedHint:
      "Still counted, but kept out of suggestions. If one has grown since you dismissed it, restore it.",
    restore: "Restore",
    suggest: "Suggest with AI",
    suggesting: "Analyzing…",
    noMisses: "No unmatched types — the ontology covers your corpus.",
    approve: "Add",
    /* The button on the mapping tab. Deliberately not called Add -- it adds nothing, the ontology
       already has it. If both buttons said Add, "it is already there" would vanish from the UI */
    mapOver: "Use existing",
    /* Blast radius: how many predicate-less facts adopting a proposal will claim. Without this
       sentence, "Add" is just one more empty relation out of nowhere */
    willRemap: (n: number) =>
      n === 1 ? "reclassifies 1 fact" : `reclassifies ${n} facts`,
    adopted: (n: number) =>
      n === 1
        ? "Added — 1 fact reclassified"
        : `Added — ${n} facts reclassified`,
    /* Some values do not fit the type and so were not rewritten. Reporting only how many were
       rewritten is reporting the good news and hiding the bad */
    adoptedPartly: (moved: number, left: number) =>
      `Added — ${moved} reclassified, ${left} left behind (value did not fit the type)`,
    /* Undo: adopting rewrites facts in bulk; with no way back nobody dares the first click */
    undoAdopt: (key: string, n: number) =>
      `${key} added, ${n} fact${n === 1 ? "" : "s"} reclassified`,
    undoAdoptBtn: "Undo",
    reverted: (n: number) =>
      n === 1 ? "Reverted — 1 fact restored" : `Reverted — ${n} facts restored`,
    undoKeepsRelation: "The relation stays; only the facts move back.",
    /* Undo needs a second confirmation: it moves facts back in bulk in one go */
    undoTitle: "Undo this ontology change?",
    undoHint: (n: number) =>
      `${n} fact${n === 1 ? "" : "s"} will go back to “related to”. The relation itself stays — ` +
      `nothing is deleted, and you can adopt it again later.`,
    undoConfirm: "Undo",
    undoCancel: "Keep",
    /* Notification for automatic ontology extension: it may be on by default only because what
       it does is visible and can be taken back. A line in the audit ledger does not count as
       visible -- that is for verifying afterwards, not for telling you now */
    autoRanTitle: "Utopia extended this ontology from your documents",
    autoRanBody: (rels: string[], facts: number) =>
      `Added ${rels.join(", ")} · ${facts} fact${facts === 1 ? "" : "s"} reclassified`,
    autoRanOff: "Turn this off in knowledge base settings.",
    /* Batch: the common case is "these are all right", and clicking them one at a time splits a
       single decision into eight */
    addAll: (n: number) => `Add all ${n}`,
    addingAll: "Adding…",
    addAllLabel: "batch",
    addAllPartial: (keys: string[]) =>
      `Some could not be added: ${keys.join(", ")} — the rest went through.`,
    proposals: "AI proposals",
    keyHint: "lowercase_snake_case",
  },
  mapping: {
    title: "Data mapping",
    hint: "What business concepts point at in the database, and how they are computed. Ask only answers using confirmed definitions.",
    tabDefinitions: "Definitions",
    tabSources: "Data sources",
    filterAll: "All",
    filterProposed: "Pending",
    filterConfirmed: "Confirmed",
    filterRejected: "Rejected",
    searchPlaceholder: "Search concept, source or table…",
    total: (n: number) => `${n} total`,
    range: (from: number, to: number, total: number) =>
      `${from}–${to} of ${total}`,
    prev: "Previous",
    next: "Next",
    empty:
      "No definitions yet. Mount a data source, then run Explore to have an agent propose a first batch.",
    emptyFiltered: "No definitions match.",
    rejectedHint:
      "Rejected ones are listed too — otherwise “why was this concept never mapped?” has no answer.",
    colConcept: "Concept",
    colSource: "Source",
    colDefinition: "How it is computed",
    colStatus: "Status",
    derivedBadge: "Derived",
    noDefinition: "(empty)",
    approve: "Confirm",
    reject: "Reject",
    edit: "Edit",
    editTitle: "Revise definition",
    fieldTable: "Table",
    fieldExpr: "Expression",
    fieldSql: "SQL",
    fieldUnit: "Unit",
    fieldSummary: "Summary",
    fieldDerived: "Derived metric (computed, not a column)",
    save: "Save",
    cancel: "Cancel",
    needOne: "Fill in at least one of table, expression or SQL.",
    history: "Revision history",
    historyHint:
      "A full snapshot of the version before each change. Kept so “how was this number computed last quarter?” has an answer.",
    historyEmpty: "Never revised.",
    historyBy: (who: string) => `Revised by ${who}`,
    historyUnknown: "a removed user",
    sourcesHint:
      "Read-only databases mounted here. Mounting ingests the schema so Ask knows which tables exist before writing SQL.",
    mount: "Mount",
    unmount: "Unmount",
    syncSchema: "Refresh schema",
    schemaSynced: (n: number) => `Schema ingested (${n} tables)`,
    // Mounted, schema did not. **Do not call this a failed mount** — the source is mounted
    schemaFailed:
      "The data source is mounted, but its schema could not be ingested — Ask cannot see which tables exist. " +
      "This is in the alert centre; check the connection, then use Refresh schema.",
    explore: "Explore mappings",
    exploreHint:
      "An agent reads these schemas and proposes metric and dimension definitions. Proposals land in Pending; Ask uses them only once confirmed.",
    exploreQueued:
      "Exploration queued — proposals will appear under Pending. If nothing can be proposed, the alert bell will say so.",
    sourcesEmpty: "No data sources mounted.",
    sourcesNoneAvailable:
      "No data sources registered yet — ask a deployment admin to register one.",
    newConn: "Register a new connection",
  },
  review: {
    title: "Review",
    hint: "Duplicates & low-confidence facts",
    tabQueue: "Queue",
    tabHistory: "History",
    empty: "Nothing to review — the graph is clean.",
    historyEmpty: "No merges yet.",
    // Left-column category navigation
    railPending: "Awaiting your nod",
    railDuplicates: "Duplicates",
    railConflicts: "Conflicts",
    railUnconfirmed: "Unconfirmed",
    railLowConfidence: "Low confidence",
    railMappings: "Data mapping",
    railViolations: "Axioms",
    railDefects: "Ontology",
    railDecisions: "Decisions",
    railMerges: "Merges",
    categoryEmpty: "This queue is clear.",
    // The decision ledger
    decisionsTitle: "Decisions",
    decisionsHint:
      "Every review decision, by whom and when — snapshots taken at decision time, kept even after the underlying fact is gone.",
    decisionsEmpty: "No decisions recorded yet.",
    aiActor: "AI adjudicator",
    decisionActions: {
      "review.merge": "Merged",
      "review.keep": "Kept apart",
      "fact.confirm": "Confirmed",
      "fact.reject": "Rejected",
      "fact.nod_confirmed": "Confirmed a remembered fact",
      "fact.nod_rejected": "Rejected a remembered fact",
      "fact.close": "Closed",
      "conflict.close_old": "Closed old",
      "conflict.keep_both": "Kept both",
      "conflict.reject_new": "Rejected new",
      "merge.revert": "Reverted merge",
      "merge.manual": "Merged manually",
    } as Record<string, string>,
    /** Why it was escalated to a human decision. The server stores a code (with an optional
        |detail); the wording is here */
    escalated: {
      escalate_no_model: "No chat model — the adjudicator could not run",
      escalate_no_verdict: "The adjudicator returned no verdict",
      escalate_entity_changed: "The entity changed while being adjudicated",
      escalate_unsure: "The adjudicator was not confident enough",
      /* One name contains the other: equality recall cannot see it, so a short form silently
         becomes a second entity */
      contains: "One name contains the other",
      ambiguous_name: "Same name, context did not settle it",
      type_drift: "Same name arrived under a different type",
      auto_merged: "Merged by the AI adjudicator",
      kept_apart: "The AI adjudicator judged these different",
    } as Record<string, string>,
    duplicates: "Possible duplicates",
    duplicatesHint:
      "Same name, different context. The AI adjudicates clear cases in the background; the rest wait for you. Merging is always reversible.",
    stageAdjudicating: "AI adjudicating",
    stageHuman: "Needs your decision",
    similarity: (pct: number) => `${pct}% context similarity`,
    factsCount: (n: number) => `${n} facts`,
    noFacts: "No recorded facts",
    merge: "Merge",
    keep: "Keep separate",
    lowConfidence: "Low-confidence facts",
    defects: "Ontology contradicts itself",
    defectsHint:
      "Problems in the definitions themselves — no facts involved. These come first: while a definition contradicts itself, every fact-level finding that rests on it is suspect.",
    defectSymAsym: "Declared both symmetric and asymmetric",
    defectTransFunc: "Transitive and functional at once",
    defectCycle: "subClassOf runs in a circle",
    defectDisjointAncestor: "Disjoint with its own ancestor",
    defectInheritsDisjoint: "Inherits from two disjoint classes",
    defectInverseSelf: "Its own inverse — say symmetric instead",
    defectInverseNotMutual: "The inverse does not point back",
    defectSubPropertyCycle: "subPropertyOf runs in a circle",
    defectRulesDisagree: "Two rules produce contradicting derivations",
    rulesDisagreeCount: (n: number) =>
      `${n} pair(s) of derivations held back until this is settled`,
    rulesDisagreeRule: (
      a: string,
      va: string,
      b: string,
      vb: string,
      axiom: string,
    ) => `${a} on ${va} with ${b} on ${vb}, against ${axiom}`,
    defectNeverInstantiable: "no instance can ever satisfy it",
    defectFixed: "I fixed the ontology",
    defectAccepted: "Leave it",
    runInference: "Run inference",
    inferring: "Inferring…",
    inferenceNoRules:
      "No transitive or symmetric property is declared, so there is no rule to run.",
    inferenceAdded: (n: number) => `${n} facts derived`,
    inferenceRetracted: (n: number) => `${n} retracted`,
    inferenceNothing: "Nothing new to derive",
    inferenceCapped: (n: number) =>
      `${n} predicate(s) hit the per-predicate limit and were not closed fully`,
    violations: "Axiom violations",
    violationsHint:
      "Facts that contradict axioms your ontology declares. Nothing here is a guess — a predicate that declares no axioms is never checked.",
    violationSelfLoop: "Points at itself",
    violationAsymmetry: "Both directions asserted",
    violationCycle: "Cycle through the transitive chain",
    violationFunctional: "Should hold one value, holds two",
    /** A signature violation (#190 / #196): a fact's subject or object falls outside the types
     *  the predicate declares -- extraction straightens it out, while the adopt and merge paths
     *  used to slip past the check */
    violationSignature: "Subject or object outside the declared types",
    /** 0017: a derivation collides with an assertion. The card is one review; the hint points at
     *  the mistake upstream */
    violationDerived: "A derivation contradicts an assertion",
    derivedLine: (s: string, p: string, o: string) =>
      `Derived: ${s} · ${p} · ${o}`,
    derivedBy: (rule: string, via: string) => `by ${rule} on ${via}`,
    assertedLine: (t: string) => `Asserted: ${t}`,
    hintStale:
      "The assertion has no end date and the derivation starts later. It may simply have ended.",
    hintDuplicate:
      "Two entities share this name. They may be the same one.",
    hintUnsure:
      "The assertion was extracted with low confidence. Read its sentence.",
    hintReadBoth: "Read both sentences and decide which one is wrong.",
    closeAssertion: "Give the assertion an end date",
    retractAssertion: "Retract the assertion",
    seeDuplicates: "See duplicates",
    openOntology: "Open the ontology",
    letBothStand: "Let both stand",
    violationVia: (p: string) => `via ${p}`,
    violationPath: (n: number) => `${n} facts in the cycle`,
    retractFact: "Data is wrong",
    /** Two-fact and cycle violations: which exact one to retract (#202) */
    retractThis: "Retract",
    retractThisHint: "Withdraw this fact from the graph; the other one stays.",
    relaxAxiom: "Axiom is wrong",
    acceptBoth: "Both are right",
    runCheck: "Run check",
    checkNeverRun:
      "Not checked yet. Contradictions are found by asking your ontology, so a run here only reports what its axioms actually say.",
    checking: "Checking…",
    checkNoAxioms:
      "No axioms declared, so nothing could be checked. Import an ontology that declares them.",
    checkFound: (n: number) => `${n} new`,
    /** They were computed, but every one of them is already in the queue or already decided --
     *  saying "3 contradictions" while the list shows one makes people think the UI dropped
     *  something */
    checkNothingNew: "Nothing new",
    checkClean: (n: number) => `${n} facts checked, no contradictions`,
    mappings: "Data mapping",
    mappingsHint:
      "Proposed mappings from a business concept to how it is computed. Confirm one and Ask uses it instead of guessing from the schema.",
    mappingDerived: "derived",
    // Facts extracted from memories, waiting for a human nod (0015)
    pending: "Awaiting your confirmation",
    pendingHint:
      "Facts extracted from what you asked the assistant to remember. Nothing here is in the graph yet: " +
      "the sentence is kept, the facts wait for your nod. Confirm to add with the sentence as evidence; " +
      "reject and it will not be proposed again.",
    pendingNoPredicate: "The ontology has no relation for this; the word is the model's own.",
    pendingNoPredicateChip: "no relation in ontology",
    pendingSaidBy: (name: string) => `said by ${name}`,
    nodCardTitle: (n: number) =>
      n === 1
        ? "One fact extracted from this. Confirm to add it to the graph, or reject."
        : `${n} facts extracted from this. Confirm to add them to the graph, or reject.`,
    lowConfidenceHint:
      "Extracted with confidence below 75%. Confirm to trust, reject to remove from the graph (the ledger keeps the record).",
    confirm: "Confirm",
    reject: "Reject",
    confidence: (pct: number) => `${pct}%`,
    mergeHistory: "Merge history",
    mergedBy: (name: string) => `by ${name}`,
    mergedByAi: "by AI adjudicator",
    revert: "Revert",
    reverted: "Reverted",
    ongoing: "now",
    conflicts: "Temporal conflicts",
    conflictsHint:
      "Two facts claim the same single-valued relation. Clear successions close automatically; " +
      "these need a human call. Closing is reversible through the ledger.",
    conflictReason: {
      no_time: "new fact has no date",
      simultaneous: "same start date",
      low_confidence: "low confidence",
    } as Record<string, string>,
    conflictVs: "vs",
    conflictSince: (d: string) => `since ${d}`,
    closeOld: "Close old",
    closeOldAt: (d: string) => `Close old at ${d}`,
    keepBoth: "Keep both",
    rejectNew: "Reject new",
    closeAtPlaceholder: "YYYY-MM-DD",
    unconfirmed: "No longer stated",
    unconfirmedHint:
      "Every source that stated these facts has since been updated without them. " +
      "Nothing is deleted automatically — absence isn't negation. Reject extraction " +
      "errors, or close a fact that genuinely ended (pick the date it ended).",
    closeFact: "Close",
    closeFactAt: (d: string) => `Close at ${d}`,
  },
  /** Copy for the generic components (SearchSelect and the like) */
  ui: {
    noMatches: "No matches",
    keepTyping: (n: number) => `${n} more — keep typing to narrow down`,
  },
  kbset: {
    title: "Knowledge base settings",
    general: "General",
    members: "Members",
    /* The automatic ontology extension switch. The note has to make clear that what you lose by
       turning it off is **only** the doing-it-for-you, not the noticing -- otherwise users will
       think turning it off hides the unmatched signals too */
    autoExtend: "Extend the ontology automatically",
    autoExtendNote:
      "When extraction meets a relation this ontology does not have, add it and reclassify the " +
      "facts that were waiting for it. Every change is listed and can be undone. Turning this " +
      "off does not stop Utopia from noticing — the phrases still collect under Unmatched, they " +
      "just wait for you to approve them.",
    materialize: "Materialize inferences",
    materializeNote:
      "Write facts the ontology entails into the ledger — transitive chains and symmetric pairs. Off by default: a declaration can be wrong, and this one changes the graph. Derived facts are marked and can be taken back.",
    inferEvery: "Re-derive every",
    minutes: "minutes",
    lastInference: (when: string) => `last run ${when}`,
    failedJobs: (n: number) => (n === 1 ? "1 failed job" : `${n} failed jobs`),
    requeue: "Run again",
    requeued: (n: number) =>
      n === 1 ? "1 job back in the queue" : `${n} jobs back in the queue`,
    /* The corpus language. The wording has to spell out "this is not the interface language", or
       someone is certain to take it for an interface switch */
    ontologyLang: "Language of this ontology",
    ontologyLangNote:
      "Which language class and relation descriptions are written in. Those go straight " +
      "into the extraction prompt, so the reader is the model while it reads your documents — " +
      "match your documents, not your interface. Changing this does not rewrite what is " +
      "already here; it decides the language of descriptions written from now on.",
    defaultOpenLabel: "Open to everyone",
    defaultOpenNote:
      "This is the deployment's default knowledge base, so visibility is locked: every member " +
      "gets at least viewer access here, which guarantees nobody signs in to an empty screen. " +
      "It can't be deleted for the same reason. To give someone more than viewing, grant a " +
      "role under Members; for a private space, create a separate knowledge base and set it " +
      "to Restricted.",
    data: "Data",
    dataHint:
      "Mounted read-only databases this knowledge base may query from Chat. " +
      "Mounting ingests the database schema so the assistant knows the tables.",
    dataMount: "Mount",
    dataUnmount: "Unmount",
    dataSyncSchema: "Refresh schema",
    dataSchemaSynced: (n: number) => `Schema ingested (${n} tables)`,
    dataExplore: "Explore mappings",
    dataExploreHint:
      "An agent reads the schemas and proposes metric/dimension definitions — review them in Review before Chat uses them.",
    dataExploreQueued:
      "Exploration queued — proposals will appear in Review shortly.",
    dataNone: "No data sources mounted.",
    dataNoneAvailable:
      "No data sources registered yet — ask a deployment admin to register one.",
    dataNewConn: "Register a new connection",
    activity: "Activity",
    activityHint:
      "Who changed what in this knowledge base. Pure audit — records are append-only.",
    auditAllActions: "All actions",
    auditSince: "From this date",
    auditUntil: "Up to this date",
    auditClear: "Clear filters",
    auditTotal: (n: number) => `${n} events`,
    activityEmpty: "Nothing recorded yet.",
    deletedUser: "a removed user",
    // The two engine actions with an empty actor_id: automatic adjudication in the review queue,
    // and all the other background work
    adjudicator: "AI adjudicator",
    engine: "the engine",
    auditActions: {
      "entity_type.created": "created entity type",
      "entity_type.updated": "updated entity type",
      "entity_type.deleted": "deleted an entity type",
      "relation_type.created": "created relation type",
      "relation_type.updated": "updated relation type",
      "relation_type.deleted": "deleted a relation type",
      "kb.updated": "updated knowledge base settings",
      "kb.member_set": "set a member role",
      "kb.member_removed": "removed a member",
      "source.created": "created source",
      "source.updated": "updated source",
      "source.deleted": "deleted a source",
      "document.deleted": "deleted document",
      "ontology.imported": "imported an ontology",
    } as Record<string, string>,
    membersHintOpen:
      "Everyone in this deployment can already read this knowledge base, so there is no " +
      "viewer role to grant — list someone here only to give them write access. " +
      "Deployment admins always have it.",
    membersHintRestricted:
      "Only the people listed here can see this knowledge base, and their role decides " +
      "what they can change. Deployment admins always have access.",
    addMember: "Add…",
    roles: { viewer: "Viewer", editor: "Editor", admin: "Admin" },
    remove: "Remove",
    noMembers: "No per-KB roles set.",
    noWriters: "Nobody has been given write access yet.",
    save: "Save",
    saved: "Saved",
    danger: "Danger zone",
    deleteKb: "Delete this knowledge base",
    deleteHint: (name: string) =>
      `Type “${name}” to confirm. Documents, graph and sources are permanently removed.`,
    deleteBtn: "Delete permanently",
    deleteRowTitle: "Delete this knowledge base",
    deleteRowHint: "Documents, graph and sources are removed permanently.",
    deleteRowBtn: "Delete",
  },
  members: {
    title: "Deployment users",
    systemAdmin: "System admin",
    remove: "Remove",
    deactivate: "Deactivate",
    deactivateHint:
      "Cuts off access everywhere — sign-in and any token already issued. What they did stays attributed to them.",
    deactivatedTitle: "Deactivated accounts",
    deactivatedHint:
      "They cannot sign in and do not appear in any member list. What they did is still attributed to them — that is why the account is kept rather than deleted.",
    reactivate: "Restore",
    deactivateConfirm: (name: string) =>
      `Deactivate ${name}? They lose access everywhere. Their past decisions stay on record.`,
    pickUser: "Select a user to add…",
    add: "Add",
    roles: {
      owner: "Owner",
      admin: "Admin",
      editor: "Editor",
      viewer: "Viewer",
    },
  },
};

/** The structural contract of a string pack. Other packs are written `const zh: Strings = {…}`;
    miss an entry and compilation fails */
export type Strings = typeof en;
