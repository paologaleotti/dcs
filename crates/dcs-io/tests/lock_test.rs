use std::time::Duration;

use dcs_io::lock::{LockOutcome, ProjectLock};

fn tempdir() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "dcs-lock-{nanos}-{:?}",
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const STALE: Duration = Duration::from_secs(300);

#[test]
fn first_instance_acquires() {
    let dir = tempdir();
    let (lock, outcome) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(outcome, LockOutcome::Acquired);
    assert!(lock.is_owned());
}

#[test]
fn second_live_instance_is_read_only() {
    let dir = tempdir();
    let (_first, a) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(a, LockOutcome::Acquired);
    let (second, b) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(
        b,
        LockOutcome::HeldByOther,
        "fresh lock held by the live first instance"
    );
    assert!(!second.is_owned());
}

#[test]
fn stale_lock_is_reclaimed() {
    let dir = tempdir();
    let (_first, _) = ProjectLock::acquire(&dir, STALE);
    // A zero-length stale window makes the existing lock instantly stale.
    let (second, outcome) = ProjectLock::acquire(&dir, Duration::from_secs(0));
    assert_eq!(
        outcome,
        LockOutcome::Acquired,
        "an abandoned lock is reclaimed"
    );
    assert!(second.is_owned());
}

#[test]
fn releasing_the_owner_frees_the_lock_for_the_next() {
    let dir = tempdir();
    {
        let (owner, outcome) = ProjectLock::acquire(&dir, STALE);
        assert_eq!(outcome, LockOutcome::Acquired);
        drop(owner); // Drop releases the owned lock
    }
    let (next, outcome) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(outcome, LockOutcome::Acquired, "lock freed on owner drop");
    assert!(next.is_owned());
}

#[test]
fn read_only_instance_does_not_release_the_owners_lock() {
    let dir = tempdir();
    let (_owner, _) = ProjectLock::acquire(&dir, STALE);
    {
        let (reader, outcome) = ProjectLock::acquire(&dir, STALE);
        assert_eq!(outcome, LockOutcome::HeldByOther);
        drop(reader); // must NOT delete the owner's lock
    }
    // Owner still holds it: a third instance is still read-only.
    let (_third, outcome) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(outcome, LockOutcome::HeldByOther);
}

#[test]
fn releasing_after_a_peer_took_over_does_not_delete_their_lock() {
    let dir = tempdir();
    let (mut owner, outcome) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(outcome, LockOutcome::Acquired);

    // A peer reclaims the (stale) lock and stamps a different token.
    let (peer, peer_outcome) = ProjectLock::acquire(&dir, Duration::from_secs(0));
    assert_eq!(peer_outcome, LockOutcome::Acquired);
    assert!(peer.is_owned());

    // The original owner releasing must not clobber the peer's lock.
    owner.release();
    let (_third, outcome) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(
        outcome,
        LockOutcome::HeldByOther,
        "peer's lock survived the stale owner's release"
    );
}

#[test]
fn take_over_claims_ownership() {
    let dir = tempdir();
    let (_owner, _) = ProjectLock::acquire(&dir, STALE);
    let (mut reader, outcome) = ProjectLock::acquire(&dir, STALE);
    assert_eq!(outcome, LockOutcome::HeldByOther);
    reader.take_over();
    assert!(reader.is_owned(), "take over grabs write ownership");
}

/// Many instances racing to reclaim one stale lock: at most one may win.
/// Guards the claim-then-create scheme — the old write-then-read-back could
/// hand ownership to two racers whose writes both preceded both read-backs.
#[test]
fn concurrent_stale_reclaim_has_at_most_one_winner() {
    use std::sync::Barrier;

    for _ in 0..20 {
        let dir = tempdir();
        // A stale stamp from a "crashed" instance.
        std::fs::write(dir.join("lock"), "1 42").unwrap();

        let threads = 8;
        let barrier = std::sync::Arc::new(Barrier::new(threads));
        let owners: Vec<bool> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let dir = dir.clone();
                    s.spawn(move || {
                        barrier.wait();
                        let (lock, outcome) = ProjectLock::acquire(&dir, STALE);
                        // Hold to the end so a winner's Drop can't free the
                        // lock for a late-arriving second winner.
                        let owned = outcome == LockOutcome::Acquired && lock.is_owned();
                        std::mem::forget(lock);
                        owned
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let winners = owners.iter().filter(|&&o| o).count();
        assert!(winners <= 1, "single-writer violated: {winners} owners");
    }
}
