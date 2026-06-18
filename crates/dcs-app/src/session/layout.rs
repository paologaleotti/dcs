use std::collections::HashMap;

use dcs_domain::cull::AcceptState;
use dcs_domain::grouping::{self, Axis, TimeGranularity};
use dcs_domain::photo::PhotoId;
use dcs_domain::sort::Sort;
use dcs_domain::timezone;
use time_tz::Tz;

use super::{Session, VerdictFilter, VisibleGroup};

impl Session {
    pub fn filter(&self) -> VerdictFilter {
        self.filter
    }

    /// Switch the verdict view. Recomputes the visible order and rewinds the
    /// background fill so any newly-visible photos decode.
    pub fn set_filter(&mut self, filter: VerdictFilter) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.bg_cursor = 0;
        self.rebuild_visible();
    }

    /// The visible groups (post-filter spans) the grid draws headers from.
    pub fn groups(&self) -> &[VisibleGroup] {
        &self.visible_groups
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    pub fn sort(&self) -> Sort {
        self.sort
    }

    /// The granularity actually in effect, with `Auto` resolved against the data
    /// — what the UI shows as `groups: auto (day)`.
    pub fn resolved_granularity(&self) -> Option<TimeGranularity> {
        self.resolved_gran
    }

    /// Change the grouping axis — a derived display setting; regroups.
    pub fn set_axis(&mut self, axis: Axis) {
        if self.axis == axis {
            return;
        }
        self.axis = axis;
        self.bg_cursor = 0;
        self.regroup();
    }

    /// Change the sort key/direction; regroups (group order + members).
    pub fn set_sort(&mut self, sort: Sort) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        self.bg_cursor = 0;
        self.regroup();
    }

    /// Refresh after an owned-state change (verdict or tag). Under the tag axis a
    /// mutation can change band membership, so regroup; otherwise the bands are
    /// stable and only the filtered visible order needs rebuilding.
    pub(super) fn refresh_after_owned_change(&mut self) {
        if self.axis == Axis::Tag {
            self.regroup();
        } else {
            self.rebuild_visible();
        }
    }

    /// Recompute the grouping over the whole pool, then the visible order.
    /// Called when the pool, axis, sort, or shoot zone changes.
    pub(super) fn regroup(&mut self) {
        let camera = self.resolve_camera_zone();
        let display = self.resolve_display_zone();
        self.resolved_gran = match self.axis {
            Axis::Time(g) => Some(grouping::resolve_auto(
                self.builder.photos(),
                camera,
                display,
                g,
            )),
            Axis::Tag | Axis::None => None,
        };
        let groups = match self.axis {
            Axis::Tag => self.derive_tag_groups(),
            _ => grouping::group(self.builder.photos(), self.axis, camera, display, self.sort),
        };
        self.order = groups
            .iter()
            .flat_map(|g| g.members.iter().copied())
            .collect();
        self.groups = groups;
        self.pool_revision = self.builder.revision();
        self.rebuild_visible();
    }

    /// Filter the grouped order into the visible order and rebuild the per-group
    /// spans the grid headers read. Walks groups in order so spans and cells
    /// stay in lockstep; groups with no surviving members are omitted.
    pub(super) fn rebuild_visible(&mut self) {
        let filter = self.filter;
        let photos = self.builder.photos();
        let cull = &self.cull;
        let passes = |i: usize| {
            // v1 can't decode a RAW, so a RAW-only photo has nothing to show:
            // keep it in the pool (paired, persisted, ready for RAW decode later)
            // but out of the grid. A paired photo displays via its JPEG.
            if photos[i].is_raw_only() {
                return false;
            }
            let state = cull.state(photos[i].id);
            match filter {
                VerdictFilter::All => true,
                VerdictFilter::Unreviewed => state == AcceptState::Unreviewed,
                VerdictFilter::Accepted => state == AcceptState::Accepted,
                VerdictFilter::Rejected => state == AcceptState::Rejected,
            }
        };
        let mut visible = Vec::new();
        let mut spans = Vec::new();
        for g in &self.groups {
            let start = visible.len();
            visible.extend(g.members.iter().copied().filter(|&i| passes(i)));
            let count = visible.len() - start;
            if count > 0 {
                spans.push(VisibleGroup {
                    title: g.title.clone(),
                    kind: g.kind,
                    start,
                    count,
                    total: g.members.len(),
                });
            }
        }
        self.visible = visible;
        self.visible_groups = spans;
        self.sel.clamp_focus(self.visible.len());
    }

    /// Resolve the display (shoot) zone for derivation: the configured IANA zone,
    /// else the system zone, else UTC. Domain stays pure — it only ever sees a
    /// concrete `Tz`; the system lookup (an environment read) lives here.
    pub(super) fn resolve_display_zone(&self) -> &'static Tz {
        Self::resolve_zone_or_system(self.config.shoot_zone.as_deref())
    }

    /// Resolve the camera zone used to anchor a naive EXIF time lacking an offset:
    /// the configured IANA zone, else the system zone, else UTC.
    pub(super) fn resolve_camera_zone(&self) -> &'static Tz {
        Self::resolve_zone_or_system(self.config.camera_zone.as_deref())
    }

    fn resolve_zone_or_system(name: Option<&str>) -> &'static Tz {
        name.and_then(timezone::zone)
            .or_else(|| time_tz::system::get_timezone().ok())
            .unwrap_or_else(|| timezone::zone("UTC").expect("UTC is always present"))
    }

    /// The visible order as stable ids, for selection/nav. Allocates — only
    /// called on input events, never on the per-frame paint path.
    pub(super) fn visible_ids(&self) -> Vec<PhotoId> {
        let photos = self.builder.photos();
        self.visible.iter().map(|&i| photos[i].id).collect()
    }

    /// Map each pool index to its derived group title (for `GroupAsFolders` and
    /// `{group}`). The empty stream title (axis `none`) maps to nothing.
    pub(super) fn group_titles(&self) -> HashMap<usize, &str> {
        let mut map = HashMap::new();
        for group in &self.groups {
            if group.title.is_empty() {
                continue;
            }
            for &member in &group.members {
                map.insert(member, group.title.as_str());
            }
        }
        map
    }
}
