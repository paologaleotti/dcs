//! AI semantic search: enabling the local model, the background embedding sweep,
//! and turning a typed query into a `Search` chip's matching set.
//!
//! The conductor's role here is orchestration only — the model inference lives in
//! `dcs-io` behind the `Embedder` trait, the ranking math in `dcs-domain::search`.
//! This module owns the *derived* search index (photo → embedding) and the
//! query → matches sets that feed the filter. None of it is persisted.

use std::collections::{HashMap, HashSet};
use std::thread;

use crossbeam_channel::{Receiver, TryRecvError, unbounded};
use dcs_domain::filter::FilterChip;
use dcs_domain::fingerprint::ContentFingerprint;
use dcs_domain::search::{SearchParams, rank};
use dcs_io::cache::EmbeddingCache;
use dcs_io::embedding::{EmbedRequest, EmbedResult, Embedder, SiglipEmbedder};

use super::{AiStatus, Session};

/// An in-flight "enable AI search": the embedded model is loaded on a background
/// thread (it's ~390 MB to parse + upload), the embedder handed back when ready.
pub(super) struct AiInit {
    result_rx: Receiver<Result<Box<dyn Embedder>, String>>,
}

impl Session {
    /// Where AI search stands — the search UI reads this to show the gate,
    /// load/index progress, or a live search field.
    pub fn ai_status(&self) -> &AiStatus {
        &self.ai_status
    }

    /// Whether AI search is enabled for the open project (a persisted preference).
    pub fn ai_enabled(&self) -> bool {
        self.config.ai_search_enabled == Some(true)
    }

    /// Whether an active search is still resolving — the model is loading or
    /// indexing, or a query's text embedding hasn't landed yet. The UI
    /// shows "searching…" instead of "no matches" while this holds, so an
    /// in-progress search isn't mistaken for an empty result.
    pub fn is_search_pending(&self) -> bool {
        let queries = self.active_search_queries();
        if queries.is_empty() {
            return false;
        }
        if matches!(
            self.ai_status,
            AiStatus::Loading | AiStatus::Indexing { .. }
        ) {
            return true;
        }
        // Ready, but a query typed this instant may not have embedded yet.
        queries.iter().any(|q| !self.search_vecs.contains_key(q))
    }

    /// Opt in to AI search for this project: persist the preference, then load the
    /// model. A read-only project can't change the preference. The index sweep is
    /// deferred to [`Self::maybe_start_indexing`] (after thumbnails warm).
    pub fn enable_ai_search(&mut self) {
        if self.read_only {
            return;
        }
        if self.config.ai_search_enabled != Some(true) {
            self.config.ai_search_enabled = Some(true);
            self.dirty = true;
        }
        self.start_embedder();
    }

    /// Turn AI search off for this project: persist the preference and drop the
    /// per-project index. The loaded model stays (it's global; another project may
    /// use it).
    pub fn disable_ai_search(&mut self) {
        if self.read_only {
            return;
        }
        if self.config.ai_search_enabled != Some(false) {
            self.config.ai_search_enabled = Some(false);
            self.dirty = true;
        }
        // Drop any active search chip first (clears search_sets/vecs + rebuilds), so
        // a now-unbacked chip doesn't silently blank the grid.
        self.clear_search_chips();
        self.embeddings.clear();
        self.fp_to_id.clear();
        self.index_started = false;
        self.ai_status = AiStatus::Disabled;
    }

    /// Load (or reuse) the embedder. Idempotent and free of preference/dirty
    /// writes, so both [`Self::enable_ai_search`] and the auto-enable on open call
    /// it. The model is embedded in the binary; loading it (parse + GPU upload) is
    /// done off-thread so the UI never blocks.
    pub(super) fn start_embedder(&mut self) {
        if self.embedder.is_some() {
            // Already loaded (this session, maybe by another project) — go straight
            // to the indexing state; the sweep fires once thumbnails are warm.
            self.ai_status = AiStatus::Indexing {
                done: self.embeddings.len(),
                total: self.embeddable_count(),
            };
            return;
        }
        if self.ai_init.is_some() {
            return;
        }
        let (result_tx, result_rx) = unbounded::<Result<Box<dyn Embedder>, String>>();

        let spawned = thread::Builder::new()
            .name("dcs-ai-enable".into())
            .spawn(move || {
                let result = SiglipEmbedder::new()
                    .map(|e| Box::new(e) as Box<dyn Embedder>)
                    .map_err(|e| e.to_string());
                let _ = result_tx.send(result);
            });

        match spawned {
            Ok(_) => {
                self.ai_status = AiStatus::Loading;
                self.ai_init = Some(AiInit { result_rx });
            }
            Err(e) => self.ai_status = AiStatus::Error(format!("couldn't start AI worker: {e}")),
        }
    }

    /// Run a text query, **replacing** any current search (plain Enter): the chip
    /// appears at once and matches fill in when the query embeds. No-op when AI
    /// search is off or the query is blank.
    pub fn run_search(&mut self, query: String) {
        if !self.ai_enabled() {
            return;
        }
        let query = query.trim().to_string();
        if query.is_empty() {
            return;
        }
        if let Some(embedder) = self.embedder.as_ref() {
            embedder.embed_text(self.epoch, query.clone());
        }
        self.set_search_chip(query);
    }

    /// Run a text query, **chaining** it to the current search (Shift+Enter): adds
    /// an OR'd search chip rather than replacing. No-op when off or blank.
    pub fn append_search(&mut self, query: String) {
        if !self.ai_enabled() {
            return;
        }
        let query = query.trim().to_string();
        if query.is_empty() {
            return;
        }
        if let Some(embedder) = self.embedder.as_ref() {
            embedder.embed_text(self.epoch, query.clone());
        }
        self.add_search_chip(query);
    }

    /// Start the embedding sweep once — but only after the scan settles and every
    /// displayable thumbnail is warm, so photo loading always wins the CPU. Fires
    /// at most once per folder (reset on open/rescan).
    pub(super) fn maybe_start_indexing(&mut self) {
        if self.index_started
            || !self.ai_enabled()
            || self.embedder.is_none()
            || self.is_scanning()
            || self.embeddable_count() == 0
            || self.import_progress().is_some()
        {
            return;
        }
        self.index_started = true;
        self.index_pool();
        // Embed any searches the user typed while waiting (their text never queued
        // because the embedder/index wasn't ready yet).
        self.ensure_query_embeds();
    }

    /// Queue a text embed for every active search query that hasn't been embedded
    /// yet — covers queries entered before the embedder was ready.
    fn ensure_query_embeds(&mut self) {
        let Some(embedder) = self.embedder.as_ref() else {
            return;
        };
        for query in self.active_search_queries() {
            if !self.search_vecs.contains_key(&query) {
                embedder.embed_text(self.epoch, query);
            }
        }
    }

    /// Seed the in-memory index from the cache and queue an embed for every
    /// displayable photo that isn't cached yet — the background sweep. Called once
    /// the pool is final and whenever the embedder becomes ready.
    pub(super) fn index_pool(&mut self) {
        let Some(embedder) = self.embedder.as_ref() else {
            return;
        };
        let model_id = embedder.model_id();
        let cached: HashMap<_, Vec<f32>> = self
            .cache
            .as_ref()
            .and_then(|c| c.lock().ok().map(|g| g.all_embeddings(model_id)))
            .map(|pairs| pairs.into_iter().collect())
            .unwrap_or_default();

        self.fp_to_id.clear();
        self.embeddings.clear();
        let mut to_embed: Vec<EmbedRequest> = Vec::new();
        for photo in self.builder.photos() {
            if photo.missing || photo.is_raw_only() {
                continue;
            }
            self.fp_to_id.insert(photo.fingerprint, photo.id);
            if let Some(vec) = cached.get(&photo.fingerprint) {
                self.embeddings.insert(photo.id, vec.clone());
            } else if let Some(path) = photo.decodable_path() {
                to_embed.push(EmbedRequest {
                    epoch: self.epoch,
                    fingerprint: photo.fingerprint,
                    path: path.to_path_buf(),
                    orientation: photo.orientation,
                });
            }
        }
        for req in to_embed {
            embedder.embed_image(req);
        }
        // Drop dead rows now that we know the live set: stale-model vectors and
        // orphans whose photo left the folder. Keeps the table proportional to
        // the pool, not its whole history.
        if let Some(cache) = self.cache.as_ref()
            && let Ok(guard) = cache.lock()
        {
            let keep: HashSet<ContentFingerprint> = self.fp_to_id.keys().copied().collect();
            guard.prune_embeddings(model_id, &keep);
        }
        self.refresh_index_status();
    }

    /// Drain the background AI work: advance an in-flight enable, then absorb any
    /// finished embeddings (caching photo vectors, resolving query matches).
    pub(super) fn poll_ai(&mut self) {
        self.poll_ai_init();
        self.poll_embed_results();
    }

    fn poll_ai_init(&mut self) {
        let result = {
            let Some(init) = self.ai_init.as_ref() else {
                return;
            };
            init.result_rx.try_recv()
        };
        match result {
            Ok(Ok(embedder)) => {
                self.embedder = Some(embedder);
                self.ai_init = None;
                // Don't sweep yet — `maybe_start_indexing` waits for thumbnails.
                self.ai_status = AiStatus::Indexing {
                    done: self.embeddings.len(),
                    total: self.embeddable_count(),
                };
                // Embed any queries typed while the model was still loading — their
                // text never queued (no embedder yet), and indexing may already have
                // run this folder so `maybe_start_indexing` won't fire again.
                self.ensure_query_embeds();
            }
            Ok(Err(e)) => {
                self.ai_status = AiStatus::Error(e);
                self.ai_init = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.ai_status = AiStatus::Error("AI worker stopped unexpectedly".into());
                self.ai_init = None;
            }
        }
    }

    fn poll_embed_results(&mut self) {
        let (model_id, results) = match self.embedder.as_ref() {
            Some(e) => (e.model_id(), e.poll()),
            None => return,
        };
        if results.is_empty() {
            return;
        }
        let mut images: Vec<(ContentFingerprint, Vec<f32>)> = Vec::new();
        let mut to_recompute: HashSet<String> = HashSet::new();
        for result in results {
            match result {
                // Drop results from a folder we've since closed (epoch bumped on
                // open): the embedder is global and may still emit old-folder work.
                EmbedResult::Image { epoch, .. } | EmbedResult::Text { epoch, .. }
                    if epoch != self.epoch => {}
                EmbedResult::Image {
                    fingerprint, vec, ..
                } => images.push((fingerprint, vec)),
                EmbedResult::Text { query, vec, .. } => {
                    self.search_vecs.insert(query.clone(), vec);
                    to_recompute.insert(query);
                }
            }
        }
        // Persist all freshly-embedded photos under a single short-lived lock,
        // rather than re-locking per result on the UI thread's tick.
        if !images.is_empty()
            && let Some(cache) = self.cache.as_ref()
            && let Ok(guard) = cache.lock()
        {
            for (fingerprint, vec) in &images {
                guard.put_embedding(fingerprint, model_id, vec);
            }
        }
        let mut new_image = false;
        for (fingerprint, vec) in images {
            if let Some(&id) = self.fp_to_id.get(&fingerprint) {
                self.embeddings.insert(id, vec);
                new_image = true;
            }
        }
        // Newly indexed photos can join any already-active search.
        if new_image {
            for query in self.active_search_queries() {
                if self.search_vecs.contains_key(&query) {
                    to_recompute.insert(query);
                }
            }
        }
        if !to_recompute.is_empty() {
            for query in &to_recompute {
                self.recompute_query(query);
            }
            self.rebuild_visible();
        }
        self.refresh_index_status();
    }

    /// Rank the in-memory index against a query's cached embedding and store the
    /// matching set. Does not rebuild the grid — callers batch that.
    fn recompute_query(&mut self, query: &str) {
        let Some(qvec) = self.search_vecs.get(query).cloned() else {
            return;
        };
        let set = {
            let photos: Vec<_> = self
                .embeddings
                .iter()
                .map(|(id, v)| (*id, v.as_slice()))
                .collect();
            rank(&qvec, &photos, &SearchParams::default())
        };
        self.search_sets.insert(query.to_string(), set);
    }

    /// The queries of every active `Search` chip.
    fn active_search_queries(&self) -> Vec<String> {
        self.active_filter()
            .groups
            .iter()
            .flat_map(|g| &g.chips)
            .filter_map(|c| match c {
                FilterChip::Search(q) => Some(q.clone()),
                _ => None,
            })
            .collect()
    }

    /// Move from `Indexing` to `Ready` once every displayable photo is embedded.
    /// Leaves non-indexing states (`Disabled`, `Error`) untouched. Does nothing
    /// until the sweep has actually started — otherwise `fp_to_id` is empty and
    /// `done >= total` (0 >= 0) would flip to a premature `Ready` (e.g. when a text
    /// query embeds before `index_pool` runs), wrongly reporting "no matches".
    fn refresh_index_status(&mut self) {
        if !self.index_started || !matches!(self.ai_status, AiStatus::Indexing { .. }) {
            return;
        }
        let done = self.embeddings.len();
        let total = self.fp_to_id.len();
        self.ai_status = if done >= total {
            AiStatus::Ready
        } else {
            AiStatus::Indexing { done, total }
        };
    }
}

// Inline unit tests: the AI state machine reads/writes private `Session` fields and
// drives private methods, so it can't be exercised from the `tests/` dir. A
// `MockEmbedder` (the `Embedder` trait is the seam) makes it deterministic without
// candle or a downloaded model.
#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crossbeam_channel::unbounded;
    use dcs_domain::fingerprint::ContentFingerprint;
    use dcs_domain::photo::PhotoId;

    use super::*;

    /// Returns pre-seeded results on `poll`; embed_* are no-ops (tests seed directly).
    struct MockEmbedder {
        out: Mutex<Vec<EmbedResult>>,
    }
    impl MockEmbedder {
        fn new() -> Self {
            Self {
                out: Mutex::new(Vec::new()),
            }
        }
        fn with(results: Vec<EmbedResult>) -> Self {
            Self {
                out: Mutex::new(results),
            }
        }
    }
    impl Embedder for MockEmbedder {
        fn model_id(&self) -> &'static str {
            "mock"
        }
        fn embed_image(&self, _req: EmbedRequest) {}
        fn embed_text(&self, _epoch: u64, _query: String) {}
        fn poll(&self) -> Vec<EmbedResult> {
            std::mem::take(&mut self.out.lock().unwrap())
        }
    }

    fn fp(n: u8) -> ContentFingerprint {
        ContentFingerprint::from_bytes([n; 32])
    }

    #[test]
    fn stale_epoch_image_result_is_dropped() {
        let mut s = Session::new();
        s.epoch = 7;
        s.fp_to_id.insert(fp(1), PhotoId(1));
        s.embedder = Some(Box::new(MockEmbedder::with(vec![EmbedResult::Image {
            epoch: 6, // previous folder
            fingerprint: fp(1),
            vec: vec![1.0, 0.0],
        }])));
        s.poll_embed_results();
        assert!(
            s.embeddings.is_empty(),
            "stale-epoch result must be dropped"
        );
    }

    #[test]
    fn fresh_epoch_image_result_lands_in_index() {
        let mut s = Session::new();
        s.epoch = 7;
        s.fp_to_id.insert(fp(1), PhotoId(1));
        s.embedder = Some(Box::new(MockEmbedder::with(vec![EmbedResult::Image {
            epoch: 7,
            fingerprint: fp(1),
            vec: vec![0.0, 1.0],
        }])));
        s.poll_embed_results();
        assert_eq!(s.embeddings.get(&PhotoId(1)), Some(&vec![0.0, 1.0]));
    }

    #[test]
    fn text_result_resolves_to_matching_photos() {
        let mut s = Session::new();
        s.embeddings.insert(PhotoId(1), vec![1.0, 0.0]);
        s.embeddings.insert(PhotoId(2), vec![0.0, 1.0]);
        s.set_search_chip("cat".to_string());
        s.embedder = Some(Box::new(MockEmbedder::with(vec![EmbedResult::Text {
            epoch: s.epoch,
            query: "cat".to_string(),
            vec: vec![1.0, 0.0], // aligned with PhotoId(1)
        }])));
        s.poll_embed_results();
        let set = s.search_sets.get("cat").expect("query resolved");
        assert!(set.contains(&PhotoId(1)) && !set.contains(&PhotoId(2)));
    }

    #[test]
    fn refresh_index_status_waits_for_index_started() {
        let mut s = Session::new();
        s.ai_status = AiStatus::Indexing { done: 0, total: 0 };
        s.index_started = false;
        s.refresh_index_status();
        assert!(
            matches!(s.ai_status, AiStatus::Indexing { .. }),
            "must not flip to Ready before the sweep starts"
        );
        s.index_started = true;
        s.refresh_index_status();
        assert_eq!(s.ai_status, AiStatus::Ready);
    }

    #[test]
    fn is_search_pending_clears_once_query_embeds() {
        let mut s = Session::new();
        s.set_search_chip("dog".to_string());
        s.ai_status = AiStatus::Ready;
        assert!(s.is_search_pending(), "query not embedded yet → pending");
        s.search_vecs.insert("dog".to_string(), vec![1.0, 0.0]);
        assert!(
            !s.is_search_pending(),
            "Ready + query embedded → not pending"
        );
    }

    #[test]
    fn search_palette_action_is_gated_on_ai_enabled() {
        use crate::registry::{ActionEffect, AppAction};
        let mut s = Session::new();
        // Off by default → the shortcut is a silent no-op.
        assert_eq!(
            s.run_action(AppAction::OpenSearchPalette),
            ActionEffect::None
        );
        s.config.ai_search_enabled = Some(true);
        assert_eq!(
            s.run_action(AppAction::OpenSearchPalette),
            ActionEffect::OpenSearchPalette
        );
    }

    #[test]
    fn poll_ai_init_promotes_loaded_embedder() {
        let mut s = Session::new();
        let (tx, rx) = unbounded();
        tx.send(Ok(Box::new(MockEmbedder::new()) as Box<dyn Embedder>))
            .unwrap();
        s.ai_init = Some(AiInit { result_rx: rx });
        s.ai_status = AiStatus::Loading;
        s.poll_ai_init();
        assert!(s.embedder.is_some());
        assert!(s.ai_init.is_none());
        assert!(matches!(s.ai_status, AiStatus::Indexing { .. }));
    }
}
