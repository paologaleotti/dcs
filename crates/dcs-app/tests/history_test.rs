//! The single durable undo timeline (#10, #18): one stack reverses both verdict
//! and tag mutations, in order, with PhotoId dedup and no-op suppression.

use dcs_app::crops::CropStore;
use dcs_app::cull::Cull;
use dcs_app::history::History;
use dcs_app::tags::TagStore;
use dcs_domain::command::{Command, Patch, TagDelta};
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Color, TagId};

/// A fresh (history, cull, tags) triple plus a dispatch helper.
struct Env {
    h: History,
    c: Cull,
    t: TagStore,
    cr: CropStore,
}

impl Env {
    fn new() -> Self {
        Env {
            h: History::new(),
            c: Cull::new(),
            t: TagStore::new(),
            cr: CropStore::new(),
        }
    }
    fn run(&mut self, cmd: Command) -> Option<Patch> {
        self.h.dispatch(cmd, &mut self.c, &mut self.t, &mut self.cr)
    }
    fn undo(&mut self) -> bool {
        self.h
            .undo(&mut self.c, &mut self.t, &mut self.cr)
            .is_some()
    }
    fn redo(&mut self) -> bool {
        self.h
            .redo(&mut self.c, &mut self.t, &mut self.cr)
            .is_some()
    }
    /// Create a tag and return its id.
    fn tag(&mut self, name: &str) -> TagId {
        let patch = self
            .run(Command::CreateTag {
                name: name.into(),
                color: Color::rgb(1, 1, 1),
            })
            .unwrap();
        match patch {
            Patch::Tag(d) => d.iter().find_map(|x| match x {
                TagDelta::Created(t) => Some(t.id),
                _ => None,
            }),
            _ => None,
        }
        .unwrap()
    }
}

fn ids(xs: &[u32]) -> Vec<PhotoId> {
    xs.iter().copied().map(PhotoId).collect()
}

#[test]
fn verdict_dispatch_records_one_entry_and_undoes() {
    let mut e = Env::new();
    e.run(Command::SetState(ids(&[1, 2]), AcceptState::Accepted));
    assert_eq!(e.h.undo_depth(), 1);
    assert_eq!(e.c.state(PhotoId(1)), AcceptState::Accepted);

    assert!(e.undo());
    assert_eq!(e.c.state(PhotoId(1)), AcceptState::Unreviewed);
    assert!(!e.undo(), "nothing left");
}

#[test]
fn duplicate_ids_dedup_to_one_change_per_photo() {
    let mut e = Env::new();
    e.run(Command::SetState(ids(&[5, 5, 5]), AcceptState::Accepted));
    assert_eq!(e.h.undo_depth(), 1);
    assert!(e.undo());
    assert_eq!(e.c.state(PhotoId(5)), AcceptState::Unreviewed);
    assert!(!e.undo());
}

#[test]
fn noop_command_records_nothing() {
    let mut e = Env::new();
    assert!(
        e.run(Command::SetState(ids(&[1]), AcceptState::Unreviewed))
            .is_none()
    );
    assert!(!e.h.can_undo());

    e.run(Command::SetState(ids(&[1]), AcceptState::Accepted));
    assert!(
        e.run(Command::SetState(ids(&[1]), AcceptState::Accepted))
            .is_none()
    );
    assert_eq!(e.h.undo_depth(), 1, "redundant re-accept records nothing");
}

#[test]
fn tag_assign_undo_redo_round_trips() {
    let mut e = Env::new();
    let t = e.tag("temples"); // entry 1
    e.run(Command::AssignTag(t, ids(&[1, 2]))); // entry 2
    assert!(e.t.is_assigned(t, PhotoId(1)));
    assert_eq!(e.h.undo_depth(), 2);

    assert!(e.undo()); // undo assign
    assert!(!e.t.is_assigned(t, PhotoId(1)));
    assert!(e.t.def(t).is_some(), "tag still exists");

    assert!(e.redo()); // redo assign
    assert!(e.t.is_assigned(t, PhotoId(1)));
}

#[test]
fn undo_a_create_removes_the_tag() {
    let mut e = Env::new();
    let t = e.tag("x");
    assert!(e.undo());
    assert!(e.t.def(t).is_none(), "undoing create deletes the def");
    assert!(e.redo());
    assert!(e.t.def(t).is_some(), "redo re-creates with the same id");
}

#[test]
fn one_timeline_interleaves_verdict_and_tag_in_reverse_order() {
    let mut e = Env::new();
    let t = e.tag("a"); // 1: create
    e.run(Command::SetState(ids(&[1]), AcceptState::Accepted)); // 2: verdict
    e.run(Command::AssignTag(t, ids(&[1]))); // 3: assign

    // Undo peels newest-first: assign, then verdict, then create.
    assert!(e.undo());
    assert!(!e.t.is_assigned(t, PhotoId(1)), "assign undone first");
    assert_eq!(
        e.c.state(PhotoId(1)),
        AcceptState::Accepted,
        "verdict still set"
    );

    assert!(e.undo());
    assert_eq!(
        e.c.state(PhotoId(1)),
        AcceptState::Unreviewed,
        "verdict undone"
    );

    assert!(e.undo());
    assert!(e.t.def(t).is_none(), "create undone last");
    assert!(!e.h.can_undo());
}

#[test]
fn rename_to_existing_merges_and_is_undoable() {
    let mut e = Env::new();
    let temples = e.tag("temples");
    let shrines = e.tag("shrines");
    e.run(Command::AssignTag(shrines, ids(&[1, 2])));
    e.run(Command::AssignTag(temples, ids(&[2])));

    e.run(Command::RenameTag(shrines, "Temples".into())); // → merge
    assert!(e.t.def(shrines).is_none());
    assert_eq!(e.t.photo_count(temples), 2);

    assert!(e.undo(), "the merge is one undoable entry");
    assert!(e.t.def(shrines).is_some(), "merged tag restored");
    assert_eq!(e.t.photo_count(shrines), 2);
    assert_eq!(e.t.photo_count(temples), 1);
}

#[test]
fn new_dispatch_clears_redo() {
    let mut e = Env::new();
    e.run(Command::SetState(ids(&[1]), AcceptState::Accepted));
    assert!(e.undo());
    assert!(e.h.can_redo());
    e.run(Command::SetState(ids(&[2]), AcceptState::Rejected));
    assert!(!e.h.can_redo(), "redo branch dropped on a fresh mutation");
}

#[test]
fn stack_is_bounded() {
    let mut e = Env::new();
    for i in 0..1500u32 {
        let target = if i % 2 == 0 {
            AcceptState::Accepted
        } else {
            AcceptState::Rejected
        };
        e.run(Command::SetState(ids(&[i]), target));
    }
    assert!(e.h.undo_depth() <= 1000, "bounded: {}", e.h.undo_depth());
    assert!(e.undo(), "newest entries remain undoable");
}

#[test]
fn forget_scrubs_photo_deltas_but_keeps_tag_defs() {
    let mut e = Env::new();
    let t = e.tag("a"); // create (no photo)
    e.run(Command::AssignTag(t, ids(&[1, 2]))); // assign photos 1,2

    e.h.forget(&[PhotoId(1)].into_iter().collect());

    // The assign entry kept only photo 2's delta; the create entry survives.
    let (undo, _redo) = e.h.stacks();
    assert_eq!(undo.len(), 2, "no entry emptied to extinction");
    match &undo[1] {
        Patch::Tag(d) => {
            assert!(
                d.iter()
                    .all(|x| !matches!(x, TagDelta::Assigned(_, PhotoId(1)))),
                "photo 1's assign scrubbed"
            );
            assert!(
                d.iter()
                    .any(|x| matches!(x, TagDelta::Assigned(_, PhotoId(2))))
            );
        }
        _ => panic!("expected a tag patch"),
    }
}

#[test]
fn from_stacks_seeds_undo_and_redo() {
    let undo = vec![Patch::Verdict(vec![(
        PhotoId(1),
        AcceptState::Unreviewed,
        AcceptState::Accepted,
    )])];
    let h = History::from_stacks(undo, vec![]);
    assert_eq!(h.undo_depth(), 1);
    assert!(h.can_undo());
}

#[test]
fn crop_dispatch_undo_redo_round_trips() {
    use dcs_domain::crops::{CropEdit, NormRect};

    let mut e = Env::new();
    let edit = CropEdit {
        angle_deg: 4.0,
        rect: NormRect::centered(0.6, 0.6),
    };
    // Set a crop on photo 1.
    let patch = e
        .run(Command::SetCrop(vec![PhotoId(1)], Some(edit)))
        .expect("a real crop records a patch");
    assert!(matches!(patch, Patch::Crop(ref c) if c.len() == 1));
    assert_eq!(e.cr.crop_of(PhotoId(1)), Some(edit));

    // Undo clears it; redo restores it.
    assert!(e.undo());
    assert_eq!(e.cr.crop_of(PhotoId(1)), None);
    assert!(e.redo());
    assert_eq!(e.cr.crop_of(PhotoId(1)), Some(edit));
}

#[test]
fn setting_the_same_crop_again_is_a_noop() {
    use dcs_domain::crops::{CropEdit, NormRect};

    let mut e = Env::new();
    let edit = CropEdit {
        angle_deg: 0.0,
        rect: NormRect::centered(0.5, 0.5),
    };
    assert!(
        e.run(Command::SetCrop(vec![PhotoId(1)], Some(edit)))
            .is_some()
    );
    // Re-applying the identical crop moves nothing → no undo entry.
    assert!(
        e.run(Command::SetCrop(vec![PhotoId(1)], Some(edit)))
            .is_none()
    );
}

#[test]
fn crop_dedups_duplicate_photo_ids() {
    use dcs_domain::crops::{CropEdit, NormRect};

    let mut e = Env::new();
    let edit = CropEdit {
        angle_deg: 1.0,
        rect: NormRect::centered(0.7, 0.7),
    };
    let patch = e
        .run(Command::SetCrop(
            vec![PhotoId(1), PhotoId(1), PhotoId(1)],
            Some(edit),
        ))
        .expect("records once");
    match patch {
        Patch::Crop(c) => assert_eq!(c.len(), 1, "deduped to a single photo"),
        _ => panic!("expected a crop patch"),
    }
}
