use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dcs_domain::contact_sheet::{self, ContactSheetPlan, ContactSheetSettings, SheetItem};
use dcs_domain::cull::AcceptState;
use dcs_domain::export::{self, ExportItem, ExportPlan, ExportRequest};
use dcs_domain::photo::{Photo, PhotoId};
use dcs_io::contact_sheet::{ContactSheetEvent, SheetThumbSource, ThumbSrc, run_contact_sheet};
use dcs_io::export::{ExportEvent, run_export};
use dcs_io::persistence::{PhotoRecord, ProjectSnapshot, ProjectStore};
use dcs_io::undo_log::{self, UndoLog};
use dcs_io::{print, recents};

use crate::contact_sheet::ContactSheetStatus;
use crate::export::{ExportScope, ExportStatus};
use crate::selection::Selection;

use super::{SaveError, Session, UNDO_LOG_FILE, relativize};

impl Session {
    /// Owned state has changed since the last successful save (debounce).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Persist owned state if it changed since the last save; no-op when clean.
    /// The UI calls this on quit and on folder swap. "Clean" must also mean no
    /// autosave is still in flight — `autosave` optimistically clears `dirty`
    /// at request time, so trusting the flag alone would let quit proceed over
    /// an unfinished (or unreported-failed) background write.
    pub fn save_if_dirty(&mut self) -> Result<(), SaveError> {
        if !self.dirty && !self.saver.has_pending() {
            return Ok(());
        }
        self.save()
    }

    /// Persist owned state now: `project.json` written atomically (verdicts +
    /// every known photo's id/fingerprint/paths + views + config), then the
    /// durable `undo.log` compacted. The log is rebuildable, so a compaction
    /// failure is swallowed and never fails the precious save. A read-only
    /// instance can't write, so this is a no-op there.
    ///
    /// Blocks until written — the quit/folder-swap path. The write still goes
    /// through the background saver so it serializes behind any in-flight
    /// [`Session::autosave`]; two writers must never interleave on the temp
    /// file. Frame-driven saves use `autosave` instead.
    pub fn save(&mut self) -> Result<(), SaveError> {
        if self.read_only {
            return Ok(());
        }
        let sidecar = self.sidecar.clone().ok_or(SaveError::NoFolder)?;
        let snapshot = self.build_snapshot();
        match self.saver.save_blocking(sidecar.clone(), snapshot) {
            Some(result) => result?,
            // Saver thread died: degrade to a direct write (state unchanged
            // since the snapshot above, so rebuilding it is equivalent).
            None => self.store.save(&sidecar, &self.build_snapshot())?,
        }
        self.save_error = None;
        let (undo, redo) = self.history.stacks();
        let stacks = undo_log::Stacks { undo, redo };
        let log_path = sidecar.join(UNDO_LOG_FILE);
        // Close the append handle before compaction rewrites the log via
        // tmp→rename. Otherwise our handle is left on the old, now-unlinked
        // inode (Unix) — silently dropping every record appended after this
        // save — or blocks the rename entirely (Windows). Reopen onto the fresh
        // compacted file so live appends keep landing in it.
        self.log = None;
        let _ = undo_log::compact(&log_path, &stacks, undo_log::DEFAULT_ENTRY_CAP);
        self.log = UndoLog::open(&log_path).ok();
        self.refresh_lock(); // keep our lock fresh on every save
        self.dirty = false;
        Ok(())
    }

    /// Fire-and-forget save on the background saver: the UI thread never
    /// blocks on serialize + fsync. Optimistically marks clean; a failure
    /// reported back through `tick` re-dirties the session (so the debounce
    /// retries) and sets [`Session::save_error`]. Skips the undo-log
    /// compaction — that runs on the blocking [`Session::save`] at quit and
    /// folder swap, and the log is rebuildable in between.
    pub fn autosave(&mut self) {
        if self.read_only || !self.dirty {
            return;
        }
        let Some(sidecar) = self.sidecar.clone() else {
            return;
        };
        let snapshot = self.build_snapshot();
        if self.saver.request(sidecar, snapshot) {
            self.dirty = false;
            self.refresh_lock();
        } else if let Err(e) = self.save() {
            // Saver thread died: the blocking path has a direct-write fallback.
            self.save_error = Some(e.to_string());
        }
    }

    /// Why the most recent save failed, when it did. Cleared by the next
    /// successful save.
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Absorb finished background saves: a failure re-dirties (the state was
    /// optimistically marked clean at request time) and records the reason.
    pub(super) fn poll_saves(&mut self) {
        for result in self.saver.poll() {
            match result {
                Ok(()) => self.save_error = None,
                Err(e) => {
                    self.dirty = true;
                    self.save_error = Some(e.to_string());
                }
            }
        }
    }

    /// Photos whose files are currently absent on disk.
    pub fn missing_count(&self) -> usize {
        self.builder.photos().iter().filter(|p| p.missing).count()
    }

    /// Forget every missing photo, removing it and its owned state from the
    /// project — the explicit prune for files the user knows are gone for good.
    /// Returns how many were removed. A no-op when read-only.
    pub fn forget_missing(&mut self) -> usize {
        if self.read_only {
            return 0;
        }
        let ids: HashSet<PhotoId> = self
            .builder
            .photos()
            .iter()
            .filter(|p| p.missing)
            .map(|p| p.id)
            .collect();
        if ids.is_empty() {
            return 0;
        }
        let removed = ids.len();
        // Capture the focused photo's identity while `visible` still maps onto
        // the un-compacted pool: after `forget` shifts pool indices, focus can
        // only be restored correctly by id, never by stale index.
        let focus_id = self
            .sel
            .focus()
            .and_then(|i| self.visible.get(i).copied())
            .and_then(|p| self.builder.photos().get(p))
            .map(|ph| ph.id)
            .filter(|id| !ids.contains(id));
        self.builder.forget(&ids);
        self.cull.forget(&ids);
        self.tags.forget(&ids);
        self.crops.forget(&ids);
        self.boards.forget(&ids);
        // Drop stale decode generations too, so a later reclaimed PhotoId doesn't
        // inherit a bumped generation and have its fresh decodes discarded.
        self.crop_gen.retain(|id, _| !ids.contains(id));
        self.history.forget(&ids);
        self.sel = Selection::new();
        self.regroup();
        if let Some(id) = focus_id
            && let Some(idx) = self.display_index_of(id)
        {
            self.set_focus(idx, false);
        }
        self.dirty = true;
        removed
    }

    /// True while another live instance holds the write lock, or the project
    /// file exists but could not be loaded (see [`Session::load_error`]).
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Why the project file failed to load, when it did. While set the session
    /// is read-only and cannot be taken over — writing would clobber a project
    /// file that is likely newer or recoverable.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Forcibly claim the write lock (UI "Take over"); we become read-write.
    /// Refused while a load error pins the session read-only.
    pub fn take_over(&mut self) {
        if self.load_error.is_some() {
            return;
        }
        if let Some(lock) = &mut self.project_lock {
            lock.take_over();
            self.read_only = !lock.is_owned();
        }
    }

    /// Refresh our lock timestamp so other instances keep seeing us as live.
    /// The UI calls this on a heartbeat while a folder is open. A refresh that
    /// discovers a peer took the lock demotes this session to read-only.
    pub fn refresh_lock(&mut self) {
        if let Some(lock) = &self.project_lock {
            lock.refresh();
            if !lock.is_owned() {
                self.read_only = true;
            }
        }
    }

    /// Enable the app-global recents store at its default location
    /// (`~/.dcs/recents.json`), loading the existing list and pruning folders
    /// that no longer exist. The UI calls this once at startup; tests leave it
    /// off so they never touch the real file.
    pub fn enable_default_recents(&mut self) {
        self.set_recents_path(recents::recents_path());
        self.recents.retain_existing();
        self.persist_recents();
    }

    /// Redirect or disable the app-global recents store. `None` disables
    /// persistence entirely (and clears the in-memory list). Tests that exercise
    /// recents point this at a temp file.
    pub fn set_recents_path(&mut self, path: Option<PathBuf>) {
        self.recents = path.as_deref().map(recents::load).unwrap_or_default();
        self.recents_path = path;
    }

    /// Recent project folders, most-recent first (app-global).
    pub fn recent_projects(&self) -> &[PathBuf] {
        &self.recents.projects
    }

    /// Clear the recent-projects list and persist the change.
    pub fn clear_recents(&mut self) {
        self.recents.clear();
        self.persist_recents();
    }

    /// The persisted grid cell size, if any.
    pub fn grid_zoom(&self) -> Option<f32> {
        self.config.grid_zoom
    }

    /// Persist the grid zoom; marks the project dirty so it saves on debounce.
    pub fn set_grid_zoom(&mut self, zoom: f32) {
        if self.read_only || self.config.grid_zoom == Some(zoom) {
            return;
        }
        self.config.grid_zoom = Some(zoom);
        self.dirty = true;
    }

    /// Whether the grid paints the burst overlay. A persisted view preference;
    /// defaults to off — bursts are an opt-in lens, not always-on chrome.
    pub fn show_bursts(&self) -> bool {
        self.config.show_bursts.unwrap_or(false)
    }

    /// Flip the burst overlay on/off; persists the preference and re-derives so
    /// the change shows at once. Read-only projects don't persist it.
    pub fn toggle_bursts(&mut self) {
        if self.read_only {
            return;
        }
        self.config.show_bursts = Some(!self.show_bursts());
        self.dirty = true;
        self.derive_bursts();
    }

    /// The persisted IANA shoot timezone (freeze-critical).
    pub fn shoot_zone(&self) -> Option<&str> {
        self.config.shoot_zone.as_deref()
    }

    /// Set the shoot timezone; marks the project dirty and regroups, since time
    /// derivation depends on it.
    pub fn set_shoot_zone(&mut self, zone: Option<String>) {
        if self.read_only || self.config.shoot_zone == zone {
            return;
        }
        self.config.shoot_zone = zone;
        self.dirty = true;
        self.regroup();
    }

    /// The persisted IANA camera timezone — the zone the camera clock was set to,
    /// used to anchor a naive EXIF time that carries no offset (freeze-critical).
    pub fn camera_zone(&self) -> Option<&str> {
        self.config.camera_zone.as_deref()
    }

    /// Set the camera timezone; marks the project dirty and regroups, since the
    /// derived absolute instant (and thus grouping) depends on it.
    pub fn set_camera_zone(&mut self, zone: Option<String>) {
        if self.read_only || self.config.camera_zone == zone {
            return;
        }
        self.config.camera_zone = zone;
        self.dirty = true;
        self.regroup();
    }

    /// Palette action ids in most-recently-used order, newest first.
    pub fn action_mru(&self) -> &[&'static str] {
        &self.mru
    }

    /// Record that a palette action ran, moving its id to the front of the MRU.
    pub(crate) fn note_action(&mut self, id: &'static str) {
        self.mru.retain(|&existing| existing != id);
        self.mru.insert(0, id);
    }

    /// Plan an export over `scope` with the dialog's `request`, via the pure
    /// planner. The result drives both the live preview and the run, so the
    /// dialog can never disagree with what gets copied. Pure: no disk access.
    pub fn plan_export(
        &self,
        scope: ExportScope,
        request: &ExportRequest,
    ) -> Result<ExportPlan, dcs_domain::export::ExportError> {
        let photos = self.builder.photos();
        let title_of = self.group_titles();
        let indices = self.scope_indices(scope);
        // Owned so the borrowed slices in each `ExportItem` outlive the planner
        // call. `primary_tags` (each photo's lowest-id tag) drives the `{tag}`
        // token; `sidecar_lists` carries each photo's adjacent sidecars, probed
        // only when the request opts in.
        let primary_tags: Vec<Option<String>> = indices
            .iter()
            .map(|&i| self.primary_tag_name(photos[i].id))
            .collect();
        let sidecar_lists: Vec<Vec<PathBuf>> = if request.sidecars {
            indices
                .iter()
                .map(|&i| self.sidecars_for(&photos[i]))
                .collect()
        } else {
            vec![Vec::new(); indices.len()]
        };
        let items: Vec<ExportItem> = indices
            .iter()
            .enumerate()
            .map(|(k, &i)| ExportItem {
                photo: &photos[i],
                group_title: title_of.get(&i).copied(),
                primary_tag: primary_tags[k].as_deref(),
                sidecars: &sidecar_lists[k],
                crop: self.crops.crop_of(photos[i].id),
            })
            .collect();
        let root = self.root.as_deref().unwrap_or(Path::new(""));
        export::plan_export(&items, root, request)
    }

    /// The adjacent sidecar files for a photo, probed once per session and
    /// memoized — the export dialog re-plans every frame, so the disk is stat-ed
    /// only on the first miss per photo.
    fn sidecars_for(&self, photo: &Photo) -> Vec<PathBuf> {
        if let Some(hit) = self.sidecar_cache.borrow().get(&photo.id) {
            return hit.clone();
        }
        let paths: Vec<&Path> = [photo.files.jpeg.as_deref(), photo.files.raw.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let found = dcs_io::source::adjacent_sidecars(&paths);
        self.sidecar_cache
            .borrow_mut()
            .insert(photo.id, found.clone());
        found
    }

    /// How many photos `scope` resolves to — the dialog's live per-scope count.
    pub fn export_scope_count(&self, scope: ExportScope) -> usize {
        self.scope_indices(scope).len()
    }

    /// Unreviewed photos in the pool — surfaced as the "N unreviewed excluded"
    /// honesty note when scope is `Accepted`.
    pub fn unreviewed_count(&self) -> usize {
        self.export_scope_count(ExportScope::Unreviewed)
    }

    /// Hand a planned export to the `dcs-io` executor and begin tracking it.
    pub fn start_export(&mut self, plan: ExportPlan) {
        let total = plan.ops.len();
        self.export_handle = Some(run_export(plan));
        self.export_status = Some(ExportStatus {
            total,
            running: true,
            ..ExportStatus::default()
        });
    }

    /// Progress of the running or last-finished export, if one has started.
    pub fn export_status(&self) -> Option<ExportStatus> {
        self.export_status
    }

    /// Request cancellation of the running export.
    pub fn cancel_export(&self) {
        if let Some(handle) = &self.export_handle {
            handle.cancel();
        }
    }

    /// Forget the last export's finished status (the dialog dismissing its toast).
    pub fn clear_export_status(&mut self) {
        if self.export_handle.is_none() {
            self.export_status = None;
        }
    }

    /// Drain export-executor events into the live status; clear the handle when
    /// the run finishes so the dialog can show its completion state.
    pub(super) fn poll_export(&mut self) {
        let Some(handle) = self.export_handle.as_ref() else {
            return;
        };
        let mut events = handle.poll();
        let running = handle.is_running();
        if !running {
            // The worker finishes the loop *then* flips the done flag, so events
            // sent between the poll above and this check are still in the channel.
            // Drain once more before retiring the handle or they're lost.
            events.extend(handle.poll());
        }
        if let Some(status) = self.export_status.as_mut() {
            for event in events {
                match event {
                    ExportEvent::Copied { .. } => status.copied += 1,
                    ExportEvent::Skipped { .. } => status.skipped += 1,
                    ExportEvent::Failed { .. } => status.failed += 1,
                }
            }
            status.running = running;
        }
        if !running {
            self.export_handle = None;
        }
    }

    /// Plan a contact sheet over `scope` with the dialog's `settings`, via the
    /// pure planner. The result drives both the live preview and the render, so
    /// the sheet always matches the grid. Pure: no disk access, no pixels.
    pub fn plan_contact_sheet(
        &self,
        scope: ExportScope,
        settings: &ContactSheetSettings,
    ) -> Result<ContactSheetPlan, contact_sheet::ContactSheetError> {
        let photos = self.builder.photos();
        let indices = self.scope_indices(scope);
        // Owned so the borrowed slices in each `SheetItem` outlive the call.
        let names: Vec<String> = indices.iter().map(|&i| stem_of(&photos[i])).collect();
        let exposures: Vec<Option<String>> = indices
            .iter()
            .map(|&i| photos[i].meta.exposure_triangle())
            .collect();
        let items: Vec<SheetItem> = indices
            .iter()
            .enumerate()
            .map(|(k, &i)| SheetItem {
                id: photos[i].id,
                orientation: photos[i].orientation,
                name: &names[k],
                exposure: exposures[k].as_deref(),
            })
            .collect();
        contact_sheet::plan_contact_sheet(&items, settings)
    }

    /// Hand a planned contact sheet to the `dcs-io` renderer and begin tracking
    /// it. `open_after` opens the finished PDF in the OS viewer for printing.
    pub fn start_contact_sheet(&mut self, plan: ContactSheetPlan, open_after: bool) {
        let total = plan.pages.len();
        self.contact_sheet_dest = Some(plan.dest.clone());
        self.contact_sheet_open_after = open_after;
        let thumbs = self.sheet_thumb_source();
        self.contact_sheet_handle = Some(run_contact_sheet(plan, thumbs));
        self.contact_sheet_status = Some(ContactSheetStatus {
            total_pages: total,
            running: true,
            ..ContactSheetStatus::default()
        });
    }

    /// The decodable file + orientation for every photo that has one. The
    /// renderer looks up each frame's `PhotoId` here; RAW-only and missing photos
    /// are simply absent (drawn as placeholder frames).
    fn sheet_thumb_source(&self) -> SheetThumbSource {
        self.builder
            .photos()
            .iter()
            .filter_map(|p| {
                p.decodable_path().map(|path| {
                    (
                        p.id,
                        ThumbSrc {
                            path: path.to_path_buf(),
                            orientation: p.orientation,
                        },
                    )
                })
            })
            .collect()
    }

    /// Progress of the running or last-finished contact-sheet render.
    pub fn contact_sheet_status(&self) -> Option<ContactSheetStatus> {
        self.contact_sheet_status
    }

    /// Request cancellation of the running contact-sheet render.
    pub fn cancel_contact_sheet(&self) {
        if let Some(handle) = &self.contact_sheet_handle {
            handle.cancel();
        }
    }

    /// Forget the last render's finished status (the dialog dismissing its toast).
    pub fn clear_contact_sheet_status(&mut self) {
        if self.contact_sheet_handle.is_none() {
            self.contact_sheet_status = None;
        }
    }

    /// Drain renderer events into the live status; on finish, open the PDF in the
    /// OS viewer when requested, then clear the handle.
    pub(super) fn poll_contact_sheet(&mut self) {
        let Some(handle) = self.contact_sheet_handle.as_ref() else {
            return;
        };
        let mut events = handle.poll();
        let running = handle.is_running();
        if !running {
            events.extend(handle.poll());
        }
        if let Some(status) = self.contact_sheet_status.as_mut() {
            for event in events {
                match event {
                    ContactSheetEvent::PageRendered { .. } => status.rendered += 1,
                    ContactSheetEvent::Failed { .. } => status.failed += 1,
                }
            }
            status.running = running;
            if !running {
                status.succeeded = status.failed == 0;
            }
        }
        if !running {
            let succeeded = self.contact_sheet_status.is_some_and(|s| s.succeeded);
            if succeeded
                && self.contact_sheet_open_after
                && let Some(dest) = &self.contact_sheet_dest
            {
                print::open_in_default_app(dest);
            }
            self.contact_sheet_open_after = false;
            self.contact_sheet_dest = None;
            self.contact_sheet_handle = None;
        }
    }

    /// Snapshot every known photo (not just culled ones, and including missing
    /// placeholders) so a rename-in-place reclaims its id and a vanished file
    /// keeps its state. Paths are stored relative to the root.
    pub(super) fn build_snapshot(&self) -> ProjectSnapshot {
        let root = self.root.as_deref();
        let photos = self
            .builder
            .photos()
            .iter()
            .map(|p| PhotoRecord {
                id: p.id,
                fingerprint: p.fingerprint,
                verdict: self.cull.state(p.id),
                tags: self.tags.tags_of(p.id),
                crop: self.crops.crop_of(p.id),
                jpeg: relativize(p.files.jpeg.as_deref(), root),
                raw: relativize(p.files.raw.as_deref(), root),
            })
            .collect();
        ProjectSnapshot {
            photos,
            next_id: self.builder.next_id(),
            tags: self.tags.defs(),
            next_tag_id: self.tags.next_id(),
            views: self.boards.to_values(),
            next_view_id: self.boards.next_view_id(),
            config: self.config.clone(),
        }
    }

    /// Fold persisted photos whose files weren't scanned into missing
    /// placeholders. Runs once after the scan completes; consumed records leave
    /// `loaded_records` empty so it's idempotent.
    pub(super) fn reconcile_missing(&mut self) {
        let root = self.root.clone();
        for rec in std::mem::take(&mut self.loaded_records) {
            let abs = |rel: Option<PathBuf>| match (&root, rel) {
                (Some(r), Some(p)) => Some(r.join(p)),
                (None, p) => p,
                (_, None) => None,
            };
            self.builder
                .add_missing(rec.fingerprint, abs(rec.jpeg), abs(rec.raw));
        }
    }

    pub(super) fn remember_recent(&mut self, root: &Path) {
        self.recents.record(root.to_path_buf());
        self.persist_recents();
    }

    fn persist_recents(&self) {
        if let Some(path) = &self.recents_path {
            let _ = recents::save(path, &self.recents);
        }
    }

    /// Pool indices in `scope`, in display order so `{seq}` and the on-disk
    /// order match the sheet.
    fn scope_indices(&self, scope: ExportScope) -> Vec<usize> {
        let photos = self.builder.photos();
        // The chip filter resolves to a pool-index set; precompute it so the
        // per-index match stays O(1). Only needed for the `CurrentFilter` scope.
        let filter_set = (scope == ExportScope::CurrentFilter)
            .then(|| dcs_domain::filter::resolve(photos, &self.filter, &self.filter_ctx()));
        self.order
            .iter()
            .copied()
            .filter(|&i| {
                let id = photos[i].id;
                match scope {
                    ExportScope::Selection => self.sel.is_selected(id),
                    ExportScope::CurrentFilter => {
                        filter_set.as_ref().is_some_and(|s| s.contains(&i))
                    }
                    ExportScope::Accepted => self.cull.state(id) == AcceptState::Accepted,
                    ExportScope::Rejected => self.cull.state(id) == AcceptState::Rejected,
                    ExportScope::Unreviewed => self.cull.state(id) == AcceptState::Unreviewed,
                    ExportScope::AcceptedAndUnreviewed => {
                        self.cull.state(id) != AcceptState::Rejected
                    }
                    ExportScope::Everything => true,
                }
            })
            .collect()
    }
}

/// A photo's displayed-file stem, for the contact-sheet filename caption.
fn stem_of(photo: &Photo) -> String {
    photo
        .display_path()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
