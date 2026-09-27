//! An answer that is still being generated, and can be re-attached to.
//!
//! **One SSE stream is tied to one HTTP request, and the answer outlives the request.**
//! Generation has already moved into its own task (`api::chat`), so refreshing the page no
//! longer loses the answer; but once that stream is cut it is cut, and after a refresh all you
//! can do is wait for it to land in the database -- the stretch in between is invisible. The
//! front end hoisting the in-flight turn out of the component solves switching back and forth
//! inside one tab; **refresh, a different tab, a different device are all outside that**.
//!
//! This fills in the last piece: register it while it generates, so anyone can re-attach.
//!
//! **On attach we hand over a snapshot first, not a replay of events.** The event stream grows
//! without bound, and buffering it means keeping every delta of a whole conversation in memory;
//! whereas a snapshot's size is the size of the answer itself, which has a natural ceiling. It
//! is simpler on the client side too: overwrite current state with the snapshot, then take
//! deltas as usual, with no need to wonder "which event did I replay up to".
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

/// One SSE event: the event name + its already-serialized data.
///
/// Not `axum::response::sse::Event` -- it gives you no way to read the content back, and here we
/// both broadcast it and use it to update the snapshot.
#[derive(Clone, Debug)]
pub struct Frame {
    pub event: &'static str,
    pub data: String,
}

impl Frame {
    pub fn new(event: &'static str, data: String) -> Self {
        Self { event, data }
    }
}

/// What this answer looks like as of right now. Whoever attaches gets it first.
#[derive(Clone, Default, Debug)]
pub struct Snapshot {
    pub content: String,
    pub steps: Vec<serde_json::Value>,
    pub sources: Vec<serde_json::Value>,
}

impl Snapshot {
    /// **The snapshot is derived from the events themselves; there is no second write path.**
    /// Two ways of writing drift apart sooner or later -- that is exactly the shape this repo
    /// has tripped over again and again (one place knows the new field, the other does not)
    fn apply(&mut self, f: &Frame) {
        match f.event {
            "delta" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&f.data) {
                    if let Some(t) = v["text"].as_str() {
                        self.content.push_str(t);
                    }
                }
            }
            "step" => {
                if let Ok(v) = serde_json::from_str(&f.data) {
                    self.steps.push(v);
                }
            }
            // sources is resent in full, not appended to
            "sources" => {
                if let Ok(serde_json::Value::Array(a)) = serde_json::from_str(&f.data) {
                    self.sources = a;
                }
            }
            _ => {}
        }
    }

    pub fn to_frame(&self) -> Frame {
        Frame::new(
            "snapshot",
            json!({
                "content": self.content,
                "steps": self.steps,
                "sources": self.sources,
            })
            .to_string(),
        )
    }
}

struct Entry {
    tx: broadcast::Sender<Frame>,
    snap: Arc<RwLock<Snapshot>>,
}

/// Generations in flight, looked up by conversation.
#[derive(Default)]
pub struct Registry(RwLock<HashMap<Uuid, Entry>>);

/// The handle held for the duration of one generation. Emits events, deregisters when done.
pub struct Handle {
    conversation_id: Uuid,
    tx: broadcast::Sender<Frame>,
    snap: Arc<RwLock<Snapshot>>,
    registry: Arc<Registry>,
}

impl Handle {
    /// Emit one event: record it into the snapshot, then broadcast.
    ///
    /// **The snapshot's write lock is still held while broadcasting**, and that part is
    /// required. Merely guaranteeing "write before send" does not stop duplicates: someone who
    /// attaches between the two steps will both see this chunk in the snapshot and receive it
    /// again from the broadcast. Sending while holding the lock, with `attach` subscribing while
    /// holding the read lock, makes the two mutually exclusive -- so the moment of attaching
    /// falls either entirely before this emit or entirely after it
    pub async fn emit(&self, frame: Frame) {
        let mut snap = self.snap.write().await;
        snap.apply(&frame);
        // Having no subscribers is the normal case (the human left), not an error
        let _ = self.tx.send(frame);
    }

    /// Generation is over. **Anyone who attaches after deregistration gets "nothing running"**,
    /// and by then the answer has landed in the database, so just read it from there
    pub async fn finish(self) {
        self.registry.0.write().await.remove(&self.conversation_id);
    }
}

impl Registry {
    /// Register a generation. Registering the same conversation twice evicts the old entry --
    /// which does not happen under normal conditions, and if it does the new one wins
    pub async fn begin(self: &Arc<Self>, conversation_id: Uuid) -> Handle {
        let (tx, _) = broadcast::channel(256);
        let snap = Arc::new(RwLock::new(Snapshot::default()));
        self.0.write().await.insert(
            conversation_id,
            Entry {
                tx: tx.clone(),
                snap: snap.clone(),
            },
        );
        Handle {
            conversation_id,
            tx,
            snap,
            registry: self.clone(),
        }
    }

    /// Attach to a generation that is running: get the snapshot as of now, plus the deltas
    /// that follow it.
    ///
    /// Returns `None` = this conversation has no generation running. **That is not an error**,
    /// it is the most common case
    pub async fn attach(
        &self,
        conversation_id: Uuid,
    ) -> Option<(Snapshot, broadcast::Receiver<Frame>)> {
        let map = self.0.read().await;
        let entry = map.get(&conversation_id)?;
        // **Subscribe while holding the snapshot's read lock.** `emit` broadcasts while
        // holding the write lock, so this stretch is mutually exclusive with any emit: the
        // snapshot we get and the subscription's starting point fit together exactly, and the
        // sliver in between is neither dropped nor duplicated
        let guard = entry.snap.read().await;
        let rx = entry.tx.subscribe();
        Some((guard.clone(), rx))
    }
}
