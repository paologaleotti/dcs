//! Command-registry tests (CLAUDE.md: app changes are tested). Covers
//! state-aware catalog availability, the action→effect dispatch contract, and
//! MRU ordering that drives the palette's default order.

use dcs_app::{ActionEffect, AppAction, Session, VerdictFilter, catalog};
use dcs_domain::grouping::{Axis, TimeGranularity};
use dcs_domain::sort::{Sort, SortDir, SortKey};

/// A fresh session has no folder: file/edit actions that need state stay out of
/// the catalog, but the always-available ones are present.
#[test]
fn catalog_hides_unavailable_actions() {
    let session = Session::new();
    let ids: Vec<&str> = catalog(&session).iter().map(|e| e.action.id()).collect();

    assert!(ids.contains(&"open-folder"), "open is always available");
    assert!(ids.contains(&"about"));
    assert!(ids.contains(&"quit"));
    assert!(ids.contains(&"set-shoot-zone"));

    // Nothing to rescan, undo, redo, or act on without a folder/selection.
    assert!(!ids.contains(&"rescan"));
    assert!(!ids.contains(&"undo"));
    assert!(!ids.contains(&"redo"));
    assert!(!ids.contains(&"accept"));
    assert!(!ids.contains(&"forget-missing"));
}

/// The active view filter is a no-op, so it's omitted; the other three appear.
#[test]
fn catalog_omits_the_active_filter() {
    let mut session = Session::new();
    session.set_filter(VerdictFilter::Accepted);
    let ids: Vec<&str> = catalog(&session).iter().map(|e| e.action.id()).collect();

    assert!(!ids.contains(&"view-accepted"), "active filter is omitted");
    assert!(ids.contains(&"view-all"));
    assert!(ids.contains(&"view-unreviewed"));
    assert!(ids.contains(&"view-rejected"));
}

/// Pure app actions mutate state and report `None`; UI-coupled ones report the
/// effect the shell must perform, without touching app state they can't.
#[test]
fn run_action_returns_effects() {
    let mut session = Session::new();

    assert_eq!(
        session.run_action(AppAction::SetFilter(VerdictFilter::Rejected)),
        ActionEffect::None
    );
    assert_eq!(session.filter(), VerdictFilter::Rejected);

    assert_eq!(
        session.run_action(AppAction::OpenFolder),
        ActionEffect::PickFolder
    );
    assert_eq!(session.run_action(AppAction::ZoomIn), ActionEffect::ZoomIn);
    assert_eq!(
        session.run_action(AppAction::SetShootZone),
        ActionEffect::OpenZonePicker
    );
    assert_eq!(session.run_action(AppAction::Quit), ActionEffect::Quit);
}

/// An out-of-range recent index is handled gracefully, not a panic.
#[test]
fn open_recent_out_of_range_is_a_noop() {
    let mut session = Session::new();
    assert_eq!(
        session.run_action(AppAction::OpenRecent(99)),
        ActionEffect::None
    );
}

/// Each run moves the action to the front of the MRU; re-running an action
/// de-duplicates rather than stacking.
#[test]
fn mru_tracks_most_recent_first() {
    let mut session = Session::new();
    session.run_action(AppAction::ZoomIn);
    session.run_action(AppAction::ZoomOut);
    session.run_action(AppAction::About);
    assert_eq!(session.action_mru(), &["about", "zoom-out", "zoom-in"]);

    // Re-running ZoomIn moves it to front without duplicating.
    session.run_action(AppAction::ZoomIn);
    assert_eq!(session.action_mru(), &["zoom-in", "about", "zoom-out"]);
}

/// Grouping + sort actions dispatch into the session's derived settings.
#[test]
fn grouping_actions_change_axis_and_sort() {
    let mut session = Session::new();
    assert_eq!(
        session.run_action(AppAction::GroupBy(Axis::None)),
        ActionEffect::None
    );
    assert_eq!(session.axis(), Axis::None);

    session.run_action(AppAction::SetGranularity(TimeGranularity::Day));
    assert_eq!(session.axis(), Axis::Time(TimeGranularity::Day));

    let name_desc = Sort {
        key: SortKey::Name,
        dir: SortDir::Desc,
    };
    session.run_action(AppAction::SetSort(name_desc));
    assert_eq!(session.sort(), name_desc);
}

/// The catalog offers the grouping/sort changes, omitting the active choice.
#[test]
fn catalog_exposes_grouping_and_sort_options() {
    let session = Session::new(); // default axis Time(Auto), sort Time Asc
    let ids: Vec<&str> = catalog(&session).iter().map(|e| e.action.id()).collect();

    // Default is the auto time axis: "none" is offered, the active "auto" not.
    assert!(ids.contains(&"group-none"));
    assert!(!ids.contains(&"gran-auto"));
    assert!(ids.contains(&"gran-day"));
    // Active sort (time asc) omitted; the others offered.
    assert!(!ids.contains(&"sort-time-asc"));
    assert!(ids.contains(&"sort-name-desc"));
    // Collapse/expand all offered while grouped.
    assert!(ids.contains(&"collapse-all-groups"));
    assert!(ids.contains(&"expand-all-groups"));
}

/// Off the none axis, the granularities (the time-axis switch) are all offered
/// and "none" is hidden.
#[test]
fn catalog_offers_granularities_to_switch_onto_time() {
    let mut session = Session::new();
    session.set_axis(Axis::None);
    let ids: Vec<&str> = catalog(&session).iter().map(|e| e.action.id()).collect();
    assert!(!ids.contains(&"group-none"));
    assert!(ids.contains(&"gran-day"));
    assert!(ids.contains(&"gran-auto"));
    // No headers on the stream axis, so collapse/expand are hidden.
    assert!(!ids.contains(&"collapse-all-groups"));
}

/// The catalog is ordered MRU-first, so the palette opens on recent actions.
#[test]
fn catalog_orders_recent_actions_first() {
    let mut session = Session::new();
    session.run_action(AppAction::About);
    session.run_action(AppAction::SetShootZone);

    let ids: Vec<&str> = catalog(&session).iter().map(|e| e.action.id()).collect();
    assert_eq!(ids[0], "set-shoot-zone");
    assert_eq!(ids[1], "about");
}
