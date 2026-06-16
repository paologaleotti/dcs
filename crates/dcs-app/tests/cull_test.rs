//! Verdict dispatch + undo/redo (§2.9, #10, #18). The meat of this phase: the
//! pure-in-RAM store and reversible patches, tested without any I/O.

use dcs_app::cull::Cull;
use dcs_domain::command::Command;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;

fn set(ids: &[u32], target: AcceptState) -> Command {
    Command::SetState(ids.iter().copied().map(PhotoId).collect(), target)
}

#[test]
fn dispatch_applies_and_absent_defaults_to_unreviewed() {
    let mut cull = Cull::new();
    assert_eq!(
        cull.state(PhotoId(1)),
        AcceptState::Unreviewed,
        "absent = unreviewed"
    );

    cull.dispatch(set(&[1, 2], AcceptState::Accepted));
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Accepted);
    assert_eq!(cull.state(PhotoId(2)), AcceptState::Accepted);
    assert_eq!(
        cull.state(PhotoId(3)),
        AcceptState::Unreviewed,
        "untouched stays unreviewed"
    );

    let c = cull.counts();
    assert_eq!((c.accepted, c.rejected), (2, 0));
}

#[test]
fn undo_redo_round_trips_a_mixed_selection() {
    let mut cull = Cull::new();
    // Pre-set a mix so a single command crosses several prior states.
    cull.dispatch(set(&[1], AcceptState::Accepted));
    cull.dispatch(set(&[2], AcceptState::Rejected));
    // 3 stays unreviewed. Now accept all three in one command.
    cull.dispatch(set(&[1, 2, 3], AcceptState::Accepted));
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Accepted);
    assert_eq!(cull.state(PhotoId(2)), AcceptState::Accepted);
    assert_eq!(cull.state(PhotoId(3)), AcceptState::Accepted);

    assert!(cull.undo());
    assert_eq!(
        cull.state(PhotoId(1)),
        AcceptState::Accepted,
        "1 was already accepted"
    );
    assert_eq!(
        cull.state(PhotoId(2)),
        AcceptState::Rejected,
        "2 restored to rejected"
    );
    assert_eq!(
        cull.state(PhotoId(3)),
        AcceptState::Unreviewed,
        "3 restored to unreviewed"
    );

    assert!(cull.redo());
    assert_eq!(cull.state(PhotoId(2)), AcceptState::Accepted);
    assert_eq!(cull.state(PhotoId(3)), AcceptState::Accepted);
}

#[test]
fn duplicate_ids_dedup_to_one_change_per_photo() {
    let mut cull = Cull::new();
    cull.dispatch(set(&[5, 5, 5], AcceptState::Accepted));
    assert_eq!(cull.undo_depth(), 1, "one command → one undo entry");

    // One undo reverses the whole (deduped) entry — there is no second copy
    // hiding in the stack.
    assert!(cull.undo());
    assert_eq!(cull.state(PhotoId(5)), AcceptState::Unreviewed);
    assert!(!cull.undo(), "nothing left to undo");
}

#[test]
fn noop_command_records_nothing() {
    let mut cull = Cull::new();
    cull.dispatch(set(&[1], AcceptState::Unreviewed)); // already unreviewed
    assert!(
        !cull.can_undo(),
        "a change that moves nothing is not undoable"
    );

    cull.dispatch(set(&[1], AcceptState::Accepted));
    cull.dispatch(set(&[1], AcceptState::Accepted)); // re-accept, no change
    assert_eq!(
        cull.undo_depth(),
        1,
        "the redundant re-accept records nothing"
    );
}

#[test]
fn accept_toggles_back_to_unreviewed() {
    // The app computes the toggle target; here we model it: A on an accepted
    // photo dispatches Unreviewed (§2.9).
    let mut cull = Cull::new();
    cull.dispatch(set(&[1], AcceptState::Accepted));
    cull.dispatch(set(&[1], AcceptState::Unreviewed));
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Unreviewed);
}

#[test]
fn reject_toggles_back_to_unreviewed() {
    let mut cull = Cull::new();
    cull.dispatch(set(&[1], AcceptState::Rejected));
    cull.dispatch(set(&[1], AcceptState::Unreviewed));
    assert_eq!(cull.state(PhotoId(1)), AcceptState::Unreviewed);
}

#[test]
fn new_dispatch_clears_the_redo_stack() {
    let mut cull = Cull::new();
    cull.dispatch(set(&[1], AcceptState::Accepted));
    assert!(cull.undo());
    assert!(cull.can_redo());

    // A fresh mutation invalidates the redo branch.
    cull.dispatch(set(&[2], AcceptState::Rejected));
    assert!(!cull.can_redo(), "redo cleared on new dispatch");
    assert_eq!(cull.redo_depth(), 0);
}

#[test]
fn undo_stack_is_bounded() {
    let mut cull = Cull::new();
    // UNDO_CAP is 1000; push well past it and confirm the depth is bounded and
    // the most recent entries are still undoable.
    for i in 0..1500u32 {
        // Alternate target so each command actually changes state and records.
        let target = if i % 2 == 0 {
            AcceptState::Accepted
        } else {
            AcceptState::Rejected
        };
        cull.dispatch(set(&[i], target));
    }
    assert!(
        cull.undo_depth() <= 1000,
        "stack stays bounded: {}",
        cull.undo_depth()
    );
    assert!(cull.undo(), "the newest entries remain undoable");
}
