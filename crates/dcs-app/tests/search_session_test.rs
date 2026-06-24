//! AI search through a real session, without enabling AI (no embedder is
//! constructed, so these tests never run model inference). The guarantee: with AI
//! disabled, search is inert and v1 behavior is fully preserved — no chip, no
//! narrowing, status stays disabled. The chip primitives (set/add/clear) are
//! tested directly. Real inference is exercised manually.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use dcs_app::{AiStatus, Session, VerdictFilter};
use dcs_domain::filter::FilterChip;
use image::{Rgb, RgbImage};

fn temp_folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dcs_search_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(dir: &Path, name: &str, seed: u8) {
    let mut img = RgbImage::new(16, 16);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = Rgb([x as u8 ^ seed, y as u8, seed]);
    }
    img.save(dir.join(name)).expect("encode jpeg");
}

fn open_three(tag: &str) -> Session {
    let dir = temp_folder(tag);
    for (i, n) in ["a.jpg", "b.jpg", "c.jpg"].iter().enumerate() {
        write_jpeg(&dir, n, i as u8 + 1);
    }
    let mut session = Session::new();
    session.open_folder(dir);
    for _ in 0..3000 {
        session.tick();
        if session.photo_count() >= 3 && !session.is_scanning() {
            break;
        }
        sleep(Duration::from_millis(1));
    }
    session
}

#[test]
fn ai_search_defaults_to_disabled() {
    let session = Session::new();
    assert_eq!(*session.ai_status(), AiStatus::Disabled);
}

#[test]
fn run_search_is_a_noop_without_a_model() {
    let mut session = open_three("noop");
    assert_eq!(session.photo_count(), 3);
    assert!(!session.is_filtered());

    // No embedder is loaded → the query must not add a chip or narrow the grid.
    session.run_search("temple".to_string());
    session.tick();

    assert!(
        !session.is_filtered(),
        "search must not filter without a model"
    );
    assert_eq!(session.photo_count(), 3);
    assert_eq!(*session.ai_status(), AiStatus::Disabled);
}

#[test]
fn blank_query_never_adds_a_chip() {
    let mut session = open_three("blank");
    session.run_search("   ".to_string());
    assert!(!session.is_filtered());
}

/// Every active `Search` chip's query, in filter order.
fn search_queries(session: &Session) -> Vec<String> {
    session
        .active_filter()
        .groups
        .iter()
        .flat_map(|g| &g.chips)
        .filter_map(|c| match c {
            FilterChip::Search(q) => Some(q.clone()),
            _ => None,
        })
        .collect()
}

/// How many groups carry a search chip — should stay 1 (the shared search group).
fn search_group_count(session: &Session) -> usize {
    session
        .active_filter()
        .groups
        .iter()
        .filter(|g| g.chips.iter().any(|c| matches!(c, FilterChip::Search(_))))
        .count()
}

#[test]
fn set_search_chip_replaces_the_current_search() {
    let mut session = open_three("replace");
    session.set_search_chip("temple".to_string());
    assert_eq!(search_queries(&session), vec!["temple"]);

    session.set_search_chip("beach".to_string());
    assert_eq!(search_queries(&session), vec!["beach"]); // replaced, not chained
    assert_eq!(search_group_count(&session), 1);
}

#[test]
fn add_search_chip_chains_into_one_group() {
    let mut session = open_three("chain");
    session.set_search_chip("temple".to_string());
    session.add_search_chip("beach".to_string());

    let queries = search_queries(&session);
    assert_eq!(queries.len(), 2);
    assert!(queries.contains(&"temple".to_string()) && queries.contains(&"beach".to_string()));
    // Both live in the *same* group (OR), so they widen the set instead of ANDing
    // it to empty.
    assert_eq!(search_group_count(&session), 1);
}

#[test]
fn set_after_chain_collapses_back_to_one() {
    let mut session = open_three("collapse");
    session.set_search_chip("a".to_string());
    session.add_search_chip("b".to_string());
    session.set_search_chip("c".to_string());
    assert_eq!(search_queries(&session), vec!["c"]);
    assert_eq!(search_group_count(&session), 1);
}

#[test]
fn disabling_ai_search_clears_active_search_chips() {
    // A leftover Search chip after disabling would be backed by nothing and blank
    // the grid (or stick on "searching"). Disabling must strip it.
    let mut session = open_three("disable");
    session.set_search_chip("temple".to_string());
    assert!(session.is_filtered());
    assert_eq!(search_queries(&session), vec!["temple"]);

    session.disable_ai_search();
    assert!(!session.is_filtered(), "disable must clear search chips");
    assert!(search_queries(&session).is_empty());
}

#[test]
fn single_search_query_reflects_a_lone_search_chip() {
    let mut session = open_three("singleq");
    assert_eq!(session.single_search_query(), None);

    session.set_search_chip("temple".to_string());
    assert_eq!(session.single_search_query(), Some("temple".to_string()));
}

#[test]
fn single_search_query_none_when_chained_or_mixed() {
    let mut session = open_three("mixedq");
    // Two chained searches isn't a single search.
    session.set_search_chip("temple".to_string());
    session.add_search_chip("beach".to_string());
    assert_eq!(session.single_search_query(), None);

    // One search but a verdict chip alongside it isn't a single search either.
    session.set_search_chip("temple".to_string());
    session.set_filter(VerdictFilter::Accepted);
    assert_eq!(session.single_search_query(), None);
}

#[test]
fn tag_results_as_search_is_noop_without_a_single_search() {
    let mut session = open_three("noopq");
    // No filter at all → nothing to tag, and no tag is invented.
    session.tag_results_as_search();
    assert!(session.all_tags().is_empty());

    // A chained search isn't a single search → still a no-op.
    session.set_search_chip("temple".to_string());
    session.add_search_chip("beach".to_string());
    session.tag_results_as_search();
    assert!(session.all_tags().is_empty());
}

#[test]
fn ai_search_off_by_default_and_run_search_gated() {
    // Default project: AI disabled, so the gated entry points do nothing even
    // though the chip primitives would.
    let mut session = open_three("gated");
    assert!(!session.ai_enabled());
    session.run_search("temple".to_string());
    assert!(!session.is_filtered());
}
