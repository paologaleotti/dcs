//! `AcceptState` + `Command` shape. The domain is pure here; the substance is
//! the serialization seam — `Command` is persisted to `undo.log`, so it must
//! round-trip and stay append-only stable (§5, #18).

use dcs_domain::command::Command;
use dcs_domain::cull::AcceptState;
use dcs_domain::photo::PhotoId;

#[test]
fn unreviewed_is_the_default_verdict() {
    assert_eq!(AcceptState::default(), AcceptState::Unreviewed);
}

#[test]
fn command_round_trips_through_json() {
    let cmd = Command::SetState(
        vec![PhotoId(3), PhotoId(7), PhotoId(3)],
        AcceptState::Accepted,
    );
    let json = serde_json::to_string(&cmd).expect("serialize");
    let back: Command = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(cmd, back, "Command survives a JSON round-trip unchanged");
}

#[test]
fn every_verdict_serializes_distinctly() {
    for state in [
        AcceptState::Unreviewed,
        AcceptState::Accepted,
        AcceptState::Rejected,
    ] {
        let json = serde_json::to_string(&state).expect("serialize");
        let back: AcceptState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back);
    }
}
