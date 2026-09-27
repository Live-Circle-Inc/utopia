//! Object storage sources: S3 (and MinIO, Ceph RGW, R2 and everything else that speaks the same
//! protocol), Azure Blob, Google Cloud Storage. All three share this one module -- `object_store`
//! confines the differences to the builder, and the listing-and-fetching code does not change by
//! a single word.
//!
//! Judged by the four criteria of
//! [0013](../../docs/decisions/0013-a-source-should-hand-over-its-history.md), it is not the same
//! kind of thing as an issue tracker:
//!
//! | Criterion | Object storage |
//! |---|---|
//! | Real timestamps | `LastModified`, but that is the **moment of writing**, not the document's own time |
//! | Does it overturn itself | Overwriting the same key is a change of story, but you only see it from the diff between two syncs |
//! | Stable identity | `bucket/key`, the cleanest one of the lot |
//! | Does enterprise knowledge live there | **The strongest one** -- document dumps, archives and data-lake landing zones are all here |
//!
//! So its value is in the fourth criterion: **it is within reach**. The second one only partly
//! holds -- an issue tracker can hand over the whole change history in one go, whereas object
//! storage either has versioning turned on (which this version does not read, see below) or can
//! only accumulate it slowly, one sync at a time. That puts it in the same class as `url` / `rss`,
//! no lower.
//!
//! **Version history is out of scope for this version.** `ListObjectVersions` could fetch exactly
//! the history 0013 wants in one call, but most buckets do not have versioning enabled, and the
//! buckets that do are mostly full of objects written once and never touched again -- ingesting
//! every version costs out of all proportion to what it returns. Add it behind a switch once
//! somebody really needs it (a policy document that is revised quarterly, and you want to see
//! what changed).
//!
//! **Why `object_store` and not `aws-sdk-s3` / `rust-s3`**: measured transitive dependencies,
//! `+7` against `+43` / `+42` -- it reuses `reqwest`, `ring` and `quick-xml`, which are already
//! in the tree. And the same API also covers Azure Blob and GCS; both "more data sources" and
//! "data lakehouse" on the roadmap sit on this line, and swapping backends is just swapping a
//! builder.

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use object_store::aws::AmazonS3Builder;
use object_store::azure::MicrosoftAzureBuilder;
use object_store::gcp::GoogleCloudStorageBuilder;
// `get` lives on `ObjectStoreExt`, not on `ObjectStore` itself -- that one only has
// `get_opts`. Without this line the error reads "&dyn ObjectStore has no method get",
// and the trait name gives you no hint of it.
use object_store::{path::Path as StorePath, ObjectStore, ObjectStoreExt as _};

/// How many objects a single sync will ingest at most.
///
/// **This is not a performance concern, it is "do not suck an entire bucket in".** A bucket
/// holding a million objects is perfectly normal, and ingestion is irreversible: every object has
/// to be extracted, embedded and put into the graph. One misconfigured prefix can burn a whole
/// day's quota, and cleaning it up afterwards is far more trouble than the configuration was.
///
/// When the cap is hit we report it in the sync stats, so people can see that "there is more",
/// rather than truncating silently.
const MAX_OBJECTS_PER_SYNC: usize = 2_000;

/// Size cap for a single object. Same order of magnitude as the upload path -- anything over
/// this size is usually not a document but a data file (backups, disk images, video), and the
/// extractor can do nothing with it.
const MAX_OBJECT_BYTES: u64 = 32 * 1024 * 1024;

/// An object waiting to be ingested.
pub struct RemoteObject {
    /// `s3://bucket/key` -- `ingest_item`'s convention is that external_key is in URI form
    pub external_key: String,
    /// The filename that lands on the document; the last segment of the key
    pub filename: String,
    pub bytes: Vec<u8>,
    pub last_modified: Option<DateTime<Utc>>,
}

/// Builds a client from the source config. All three clouds share this one entry point,
/// dispatched on `kind`.
///
/// **For all three, the "bucket" is called `bucket`**, even though Azure calls it a container and
/// GCS calls it a storage bucket. Following each product's terminology in the config would leave
/// the form and the docs telling different stories, while all this layer wants is "the name of
/// the thing that holds the stuff". Credential fields keep each vendor's own names
/// (`access_key_id` / `account_key` / `service_account_key`) -- those are what the user copied
/// out of the console, and renaming them only leaves people unable to match them up.
///
/// **If `endpoint` is there it is a self-hosted deployment** (MinIO, Ceph, R2, Azurite), and in
/// that case path-style is forced for S3: virtual-hosted-style addressing needs a name like
/// `bucket.host` to resolve, and a self-hosted deployment usually has nothing but a bare IP.
/// Without forcing it the symptom is "bucket does not exist" -- while the bucket is plainly
/// sitting right there.
///
/// Allowing http is for the same case: MinIO on an internal network and local emulators often
/// run without TLS. **This is a downgrade the config asks for**, not a default -- with no
/// endpoint given it is still https only.
pub fn client(kind: &str, config: &serde_json::Value) -> anyhow::Result<Box<dyn ObjectStore>> {
    let s = |k: &str| {
        config[k]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let bucket =
        s("bucket").ok_or_else(|| anyhow::anyhow!("{kind} source is missing config.bucket"))?;
    let endpoint = s("endpoint");
    let insecure = endpoint
        .as_deref()
        .is_some_and(|e| e.starts_with("http://"));

    match kind {
        "azure_blob" => {
            let mut b = MicrosoftAzureBuilder::new().with_container_name(&bucket);
            if let Some(account) = s("account_name") {
                b = b.with_account(account);
            }
            if let Some(key) = s("account_key") {
                b = b.with_access_key(key);
            }
            if let Some(e) = endpoint {
                b = b.with_endpoint(e).with_allow_http(insecure);
            }
            Ok(Box::new(b.build().context("azure blob client")?))
        }
        "gcs" => {
            let mut b = GoogleCloudStorageBuilder::new().with_bucket_name(&bucket);
            // The whole service-account JSON is pasted in, not a path: the source config is one
            // JSONB row, and the file may well not be on the server
            let key = s("service_account_key");
            if let Some(json) = key.clone() {
                b = b.with_service_account_key(json);
            }
            if let Some(e) = endpoint {
                // **`with_base_url`, not `with_url`.** The latter is for parsing storage
                // locations like `gs://bucket/path`; feed it an http endpoint and the error is
                // "Unknown url scheme cannot be parsed" -- the names show no difference
                b = b.with_base_url(&e);
                // A self-hosted endpoint usually has no Google credential system (emulators, S3
                // gateways). With no service account given we skip signing, otherwise the builder
                // goes looking for default credentials and then fails
                if key.is_none() {
                    b = b.with_skip_signature(true);
                }
            }
            Ok(Box::new(b.build().context("gcs client")?))
        }
        // s3 and everything else that speaks the same protocol
        _ => {
            let mut b = AmazonS3Builder::new().with_bucket_name(&bucket);
            if let Some(region) = s("region") {
                b = b.with_region(region);
            }
            if let (Some(key), Some(secret)) = (s("access_key_id"), s("secret_access_key")) {
                b = b.with_access_key_id(key).with_secret_access_key(secret);
            }
            if let Some(e) = endpoint {
                b = b
                    .with_endpoint(e)
                    .with_virtual_hosted_style_request(false)
                    .with_allow_http(insecure);
            }
            Ok(Box::new(b.build().context("object storage client")?))
        }
    }
}

/// Each vendor's URI scheme, used as the prefix of external_key.
///
/// Hard-coded rather than pasted together from kind: the common spelling for GCS is gs while the
/// source type is called gcs, and Azure's container URI is not called azure_blob either. What we
/// want here is the form other people will recognise.
fn uri_scheme(kind: &str) -> &'static str {
    match kind {
        "azure_blob" => "azure",
        "gcs" => "gs",
        _ => "s3",
    }
}

/// Lists the objects under a prefix and fetches them one by one.
///
/// **Zero-byte objects are always skipped, and the test is the size, not the name.** The
/// intuitive way to write it is to look at whether the key ends in `/` -- that is how the console
/// and `aws s3 sync` make "folders". But `object_store`'s `Path` **normalises the trailing slash
/// away**: `docs/` arrives as `docs`, that check never holds, and then it goes and GETs `docs`
/// while the real key in the bucket is `docs/` -- so a 404 takes the whole sync down with it.
/// That is exactly how it blew up on MinIO.
///
/// Judging by size is also the wider net: a zero-byte object is not a document whatever it is
/// called, and `ingest_item` handed empty bytes would only return `Unchanged` anyway.
///
/// Format is not judged here. `utopia_ingest`'s extraction dispatches on extension plus mime and
/// decodes anything it cannot recognise as text -- maintaining another allowlist here would be
/// writing the same thing twice, and two such lists drift apart sooner or later.
pub async fn fetch(
    kind: &str,
    store: &dyn ObjectStore,
    bucket: &str,
    prefix: Option<&str>,
) -> anyhow::Result<(Vec<RemoteObject>, bool)> {
    // **Each of the three uses its own scheme.** Identity has to be unique: a bucket with the
    // same name on S3 and on GCS is two different things, and sharing one prefix would let the
    // documents of two sources claim each other
    let scheme = uri_scheme(kind);
    let p = prefix.map(StorePath::from);
    let mut listing = store.list(p.as_ref());
    let mut out = Vec::new();
    let mut truncated = false;
    let mut unreadable = 0usize;

    while let Some(meta) = listing.next().await {
        let meta = meta.context("listing objects")?;
        let key = meta.location.as_ref().to_string();

        if meta.size == 0 {
            continue;
        }
        if meta.size > MAX_OBJECT_BYTES {
            tracing::info!(%key, size = meta.size, "object too large, skipping");
            continue;
        }
        if out.len() >= MAX_OBJECTS_PER_SYNC {
            truncated = true;
            break;
        }

        // **One object that cannot be fetched should not take the whole sync down.** There is
        // time between the listing and the fetch, and being deleted or having its permissions
        // changed in between is normal; the bigger the bucket the more common it gets. Note it
        // down and report it all together at the end, rather than letting one 404 on the 900th
        // object throw away the results of the first 899 as well.
        let got = async {
            let r = store.get(&meta.location).await?;
            r.bytes().await
        }
        .await;
        let bytes = match got {
            Ok(b) => b,
            Err(e) => {
                unreadable += 1;
                tracing::warn!(%key, error = %e, "could not fetch object, skipping");
                continue;
            }
        };

        out.push(RemoteObject {
            external_key: format!("{scheme}://{bucket}/{key}"),
            filename: key.rsplit('/').next().unwrap_or(&key).to_string(),
            bytes: bytes.to_vec(),
            last_modified: Some(meta.last_modified),
        });
    }
    if unreadable > 0 {
        tracing::warn!(unreadable, bucket, "some objects could not be fetched");
    }
    Ok((out, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-hosted endpoint has to use path-style, otherwise the request goes to `bucket.host`
    /// and a self-hosted deployment generally has no such DNS record. The symptom is "bucket does
    /// not exist" while the bucket is right there.
    #[test]
    fn a_self_hosted_endpoint_builds() {
        let cfg = serde_json::json!({
            "bucket": "docs",
            "endpoint": "http://127.0.0.1:9000",
            "region": "us-east-1",
            "access_key_id": "minioadmin",
            "secret_access_key": "minioadmin",
        });
        assert!(client("s3", &cfg).is_ok());
    }

    /// With no bucket given it has to report which item is missing -- following the wording of
    /// the jira / github sources.
    #[test]
    fn a_missing_bucket_says_so() {
        let e = client("s3", &serde_json::json!({ "region": "us-east-1" })).unwrap_err();
        assert!(e.to_string().contains("config.bucket"), "{e}");
    }

    /// The public-cloud path: with no endpoint given it should not be forced into path-style,
    /// and plaintext http should not be allowed either.
    #[test]
    fn a_cloud_bucket_needs_no_endpoint() {
        let cfg = serde_json::json!({
            "bucket": "docs",
            "region": "ap-southeast-1",
            "access_key_id": "AKIA...",
            "secret_access_key": "...",
        });
        assert!(client("s3", &cfg).is_ok());
    }

    /// Really connects to an S3-compatible endpoint, and tests only the **read** side: listing,
    /// fetching content, and whether the identity is right.
    ///
    /// **With no `UTOPIA_S3_TEST_ENDPOINT` it skips rather than fails** -- the same convention as
    /// the tests that need a database (see CONTRIBUTING). The three above only prove the builder
    /// can be assembled; what this one guards is that **the wire protocol really works out**:
    /// whether path-style took effect, whether plaintext http was let through, whether the
    /// `s3://` identity is assembled correctly, whether `LastModified` comes back.
    /// Not one of those can be discovered while constructing the client.
    ///
    /// **The data is laid out from outside; the test only reads, it never writes.** The first
    /// version had the test `put` its own data, and `object_store`'s `Path` took a bite out of
    /// it: it normalises `docs/` into `docs`, so the "directory placeholder" got written as an
    /// ordinary object named `docs`, and MinIO does not allow the object `docs` and the prefix
    /// `docs/` to coexist -- the whole prefix was displaced and the listing came back empty.
    /// A real placeholder object can only be made by another client, so let another client make
    /// it.
    ///
    /// How to run it (start MinIO first, then lay the data out with curl; curl has SigV4 built
    /// in):
    /// ```text
    /// MINIO_ROOT_USER=u MINIO_ROOT_PASSWORD=p123456 \
    ///   minio server ./data --address 127.0.0.1:19000 &
    /// S3="curl -s --aws-sigv4 aws:amz:us-east-1:s3 -u u:p123456"
    /// B=http://127.0.0.1:19000/utopia-conn-test
    /// $S3 -X PUT $B                                  # create the bucket
    /// $S3 -X PUT --data-raw hello    "$B/docs/a.txt"
    /// $S3 -X PUT --data-raw '# world' "$B/docs/b.md"
    /// $S3 -X PUT --data-raw ''        "$B/docs/"     # directory placeholder
    /// UTOPIA_S3_TEST_ENDPOINT=http://127.0.0.1:19000 \
    ///   UTOPIA_S3_TEST_KEY=u UTOPIA_S3_TEST_SECRET=p123456 \
    ///   cargo test -p utopia-server object_storage
    /// ```
    #[tokio::test]
    async fn it_reads_from_a_real_s3_endpoint() -> anyhow::Result<()> {
        let Ok(endpoint) = std::env::var("UTOPIA_S3_TEST_ENDPOINT") else {
            eprintln!("skipped: UTOPIA_S3_TEST_ENDPOINT is not set");
            return Ok(());
        };
        let bucket = "utopia-conn-test";
        let cfg = serde_json::json!({
            "bucket": bucket,
            "endpoint": endpoint,
            "region": "us-east-1",
            "access_key_id": std::env::var("UTOPIA_S3_TEST_KEY").unwrap_or("minioadmin".into()),
            "secret_access_key": std::env::var("UTOPIA_S3_TEST_SECRET").unwrap_or("minioadmin".into()),
        });
        let store = client("s3", &cfg)?;

        let (objs, truncated) = fetch("s3", store.as_ref(), bucket, Some("docs")).await?;
        assert!(!truncated, "three objects should not trip the cap");

        let keys: Vec<&str> = objs.iter().map(|o| o.external_key.as_str()).collect();
        for want in [
            "s3://utopia-conn-test/docs/a.txt",
            "s3://utopia-conn-test/docs/b.md",
        ] {
            assert!(keys.contains(&want), "missing {want}: {keys:?}");
        }

        // **This one guards a bug that really did blow up.** The bucket holds a `docs/`
        // directory placeholder object, `object_store` normalised the trailing slash away, so it
        // entered the loop under the identity `docs`, the GET came back 404, and the whole sync
        // failed. Not one of the three client-construction unit tests can find that -- you only
        // see it by really connecting to a bucket that has had a "folder" made in it.
        assert_eq!(objs.len(), 2, "placeholder not filtered out: {keys:?}");

        let a = objs.iter().find(|o| o.filename == "a.txt").expect("a.txt");
        assert_eq!(a.bytes, b"hello", "fetched content is wrong");
        assert!(
            a.last_modified.is_some(),
            "LastModified is the only source of doc_time"
        );
        Ok(())
    }

    /// Really connects to Azure Blob once. **This and the S3 one are two different wire
    /// protocols** -- the signing algorithm, the shape of the list response and the semantics of
    /// containers versus buckets all differ, and sharing one builder abstraction does not mean
    /// sharing one path that actually works.
    ///
    /// Verified with Azurite (the official emulator), skipped with no
    /// `UTOPIA_AZURE_TEST_ENDPOINT`:
    /// ```text
    /// docker run -d -p 10000:10000 mcr.microsoft.com/azure-storage/azurite \\
    ///   azurite-blob --blobHost 0.0.0.0 --skipApiVersionCheck
    /// # lay the data out with rclone (rclone supports Azurite's dev account natively)
    /// UTOPIA_AZURE_TEST_ENDPOINT=http://127.0.0.1:10000/devstoreaccount1 \\
    ///   cargo test -p utopia-server object_storage
    /// ```
    #[tokio::test]
    async fn it_reads_from_azure_blob() -> anyhow::Result<()> {
        let Ok(endpoint) = std::env::var("UTOPIA_AZURE_TEST_ENDPOINT") else {
            eprintln!("skipped: UTOPIA_AZURE_TEST_ENDPOINT is not set");
            return Ok(());
        };
        let cfg = serde_json::json!({
            "bucket": "utopia-conn-test",
            "endpoint": endpoint,
            "account_name": "devstoreaccount1",
            // Azurite's fixed development key. A public constant, not a credential
            "account_key": "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==",
        });
        let store = client("azure_blob", &cfg)?;
        let (objs, _) = fetch(
            "azure_blob",
            store.as_ref(),
            "utopia-conn-test",
            Some("docs"),
        )
        .await?;

        let a = objs.iter().find(|o| o.filename == "a.txt").expect("a.txt");
        assert_eq!(a.bytes, b"hello from azure");
        // **Identity has to carry each vendor's own scheme**: a bucket with the same name on two
        // clouds is two different things
        assert_eq!(a.external_key, "azure://utopia-conn-test/docs/a.txt");
        assert!(a.last_modified.is_some());
        Ok(())
    }

    /// Really connects to GCS once. **There is no usable emulator, so this one is always skipped
    /// on CI.**
    ///
    /// Tried `fsouza/fake-gcs-server`, no good: it only implements the JSON API
    /// (`/storage/v1/b/...`), while `object_store`'s GCS backend lists objects through the
    /// **XML API** (`/bucket?list-type=2`), and on that path it answers 404.
    /// In other words, the most common GCS emulator cannot test the protocol path we actually use.
    ///
    /// So the GCS path is **covered by construction tests only**, which is not the same grade as
    /// S3 and Azure. Anyone with a real bucket can fill the gap in one run:
    /// ```text
    /// UTOPIA_GCS_TEST_ENDPOINT=https://storage.googleapis.com \\
    ///   UTOPIA_GCS_TEST_BUCKET=your-bucket \\
    ///   UTOPIA_GCS_TEST_KEY="$(cat service-account.json)" \\
    ///   cargo test -p utopia-server object_storage
    /// ```
    #[tokio::test]
    async fn it_reads_from_gcs() -> anyhow::Result<()> {
        let (Ok(endpoint), Ok(bucket)) = (
            std::env::var("UTOPIA_GCS_TEST_ENDPOINT"),
            std::env::var("UTOPIA_GCS_TEST_BUCKET"),
        ) else {
            eprintln!("skipped: UTOPIA_GCS_TEST_ENDPOINT / _BUCKET is not set");
            return Ok(());
        };
        let mut cfg = serde_json::json!({ "bucket": bucket, "endpoint": endpoint });
        if let Ok(k) = std::env::var("UTOPIA_GCS_TEST_KEY") {
            cfg["service_account_key"] = serde_json::Value::String(k);
        }
        let store = client("gcs", &cfg)?;
        let (objs, _) = fetch("gcs", store.as_ref(), &bucket, Some("docs")).await?;

        let a = objs.iter().find(|o| o.filename == "a.txt").expect("a.txt");
        assert_eq!(a.external_key, format!("gs://{bucket}/docs/a.txt"));
        assert!(a.last_modified.is_some());
        Ok(())
    }
}
