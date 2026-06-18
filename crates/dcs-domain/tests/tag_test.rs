//! Tag type, palette, and delta-inversion tests. Pure, no mocks.

use dcs_domain::command::{Patch, TagDelta};
use dcs_domain::photo::PhotoId;
use dcs_domain::tag::{Color, PALETTE, Tag, TagId, normalize_name, palette_color};

fn tag(id: u32, name: &str) -> Tag {
    Tag {
        id: TagId(id),
        name: name.to_string(),
        color: palette_color(id as usize),
    }
}

#[test]
fn palette_maps_one_based_slots_to_the_curated_set() {
    assert_eq!(palette_color(1), PALETTE[0]);
    assert_eq!(palette_color(PALETTE.len()), PALETTE[PALETTE.len() - 1]);
}

#[test]
fn colors_continue_past_the_curated_set() {
    // Tags are unlimited; beyond the curated palette, colors keep generating
    // distinct hues rather than wrapping or panicking. Sample a run of generated
    // slots and confirm none collides with another or with a curated color.
    let n = PALETTE.len();
    let mut seen: Vec<_> = PALETTE.to_vec();
    for slot in (n + 1)..=(n + 12) {
        let c = palette_color(slot);
        assert!(
            !seen.contains(&c),
            "generated color for slot {slot} collides with an earlier color"
        );
        seen.push(c);
    }
    // Slot 0 is never a real index; it must not panic.
    let _ = palette_color(0);
}

#[test]
fn palette_hues_are_distinct() {
    for (i, a) in PALETTE.iter().enumerate() {
        for b in &PALETTE[i + 1..] {
            assert_ne!(a, b, "palette colors must be distinct");
        }
    }
}

#[test]
fn normalize_name_trims_and_casefolds() {
    assert_eq!(normalize_name("  Temples "), "temples");
    assert_eq!(normalize_name("SHRINES"), normalize_name("shrines"));
    assert_ne!(normalize_name("temple"), normalize_name("temples"));
}

#[test]
fn color_rgb_is_plain_bytes() {
    let c = Color::rgb(0x12, 0x34, 0x56);
    assert_eq!((c.r, c.g, c.b), (0x12, 0x34, 0x56));
}

#[test]
fn delta_invert_round_trips_every_variant() {
    let p = PhotoId(7);
    let t = TagId(3);
    let cases = [
        TagDelta::Assigned(t, p),
        TagDelta::Unassigned(t, p),
        TagDelta::Created(tag(3, "temples")),
        TagDelta::Removed(tag(3, "temples")),
        TagDelta::Renamed {
            id: t,
            before: "a".into(),
            after: "b".into(),
        },
        TagDelta::Recolored {
            id: t,
            before: Color::rgb(1, 2, 3),
            after: Color::rgb(4, 5, 6),
        },
    ];
    for d in cases {
        assert_eq!(d.invert().invert(), d, "double invert is identity");
    }
}

#[test]
fn delta_invert_swaps_assign_and_rename() {
    let d = TagDelta::Assigned(TagId(1), PhotoId(2));
    assert_eq!(d.invert(), TagDelta::Unassigned(TagId(1), PhotoId(2)));

    let r = TagDelta::Renamed {
        id: TagId(1),
        before: "old".into(),
        after: "new".into(),
    };
    assert_eq!(
        r.invert(),
        TagDelta::Renamed {
            id: TagId(1),
            before: "new".into(),
            after: "old".into(),
        }
    );
}

#[test]
fn patch_emptiness() {
    assert!(Patch::Verdict(vec![]).is_empty());
    assert!(Patch::Tag(vec![]).is_empty());
    assert!(!Patch::Tag(vec![TagDelta::Assigned(TagId(1), PhotoId(1))]).is_empty());
}

#[test]
fn delta_serde_round_trip() {
    let d = TagDelta::Created(tag(5, "golden hour"));
    let json = serde_json::to_string(&d).unwrap();
    let back: TagDelta = serde_json::from_str(&json).unwrap();
    assert_eq!(d, back);
}
