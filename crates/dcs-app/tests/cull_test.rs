//! Verdict store primitives (§2.9, #10). The map and its apply/revert deltas;
//! the undo *timeline* is tested in `history_test`.

use dcs_app::cull::Cull;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;

#[test]
fn absent_defaults_to_unreviewed() {
    let cull = Cull::new();
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Unreviewed);
}

#[test]
fn apply_set_state_dedups_and_skips_noops() {
    let mut cull = Cull::new();
    let changes =
        cull.apply_set_state(&[PhotoId(5), PhotoId(5), PhotoId(5)], AcceptState::Accepted);
    assert_eq!(
        changes.len(),
        1,
        "duplicates collapse to one change per photo"
    );
    assert_eq!(cull.state(PhotoId(5)), AcceptState::Accepted);

    // Re-applying the same target moves nothing.
    let again = cull.apply_set_state(&[PhotoId(5)], AcceptState::Accepted);
    assert!(again.is_empty(), "a no-op records no delta");
}

#[test]
fn counts_tally_accepted_and_rejected() {
    let mut cull = Cull::new();
    cull.apply_set_state(&[PhotoId(1), PhotoId(2)], AcceptState::Accepted);
    cull.apply_set_state(&[PhotoId(3)], AcceptState::Rejected);
    let c = cull.counts();
    assert_eq!((c.accepted, c.rejected), (2, 1));
}

#[test]
fn apply_and_revert_move_to_after_and_before() {
    let mut cull = Cull::new();
    let changes = cull.apply_set_state(&[PhotoId(1)], AcceptState::Accepted);
    cull.revert(&changes);
    assert_eq!(
        cull.state(PhotoId(1)),
        AcceptState::Unreviewed,
        "revert → before"
    );
    cull.apply(&changes);
    assert_eq!(
        cull.state(PhotoId(1)),
        AcceptState::Accepted,
        "apply → after"
    );
}

#[test]
fn from_verdicts_drops_unreviewed() {
    let cull = Cull::from_verdicts([
        (PhotoId(1), AcceptState::Accepted),
        (PhotoId(2), AcceptState::Unreviewed),
    ]);
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Accepted);
    assert_eq!(cull.counts().accepted, 1, "unreviewed not stored");
}

#[test]
fn forget_drops_verdicts() {
    let mut cull = Cull::new();
    cull.apply_set_state(&[PhotoId(1), PhotoId(2)], AcceptState::Accepted);
    cull.forget(&[PhotoId(1)].into_iter().collect());
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Unreviewed);
    assert_eq!(cull.counts().accepted, 1);
}
