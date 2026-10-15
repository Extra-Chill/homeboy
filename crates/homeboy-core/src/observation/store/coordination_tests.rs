use std::sync::{Arc, Barrier};
use std::time::Duration;

use serde_json::json;

use super::*;

const NAMESPACE: &str = "coordination-test";
const TTL: Duration = Duration::from_secs(60);

fn store_pair() -> (tempfile::TempDir, ObservationStore, ObservationStore) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("observations.sqlite");
    let first = ObservationStore::open_initialized_for_lifecycle_at(&path).expect("open first");
    let second = ObservationStore::open_initialized_for_lifecycle_at(&path).expect("open second");
    (directory, first, second)
}

fn write<'a>(state: &'a str, payload: &'a Value) -> RecordWrite<'a> {
    RecordWrite {
        state,
        payload,
        expires_at_ms: None,
    }
}

fn exclusive<'a>(key: &'a str, owner: LeaseOwner, empty: &'a Value) -> LeaseRequest<'a> {
    LeaseRequest {
        namespace: NAMESPACE,
        key,
        ttl: TTL,
        owner,
        admission: LeaseAdmission::Exclusive,
        weight: empty,
        payload: empty,
    }
}

fn acquired(outcome: LeaseAcquire) -> LeaseHolder {
    match outcome {
        LeaseAcquire::Acquired(holder) => holder,
        LeaseAcquire::Contended(holders) => panic!("expected acquire, contended by {holders:?}"),
    }
}

fn applied(outcome: RecordOutcome) -> CoordinationRecord {
    match outcome {
        RecordOutcome::Applied(Some(record)) => record,
        other => panic!("expected an applied write, got {other:?}"),
    }
}

#[test]
fn record_writes_compare_and_set_on_revision() {
    let (_dir, first, second) = store_pair();
    let payload = json!({"step": 1});

    let created = applied(
        first
            .put_coordination_record(
                NAMESPACE,
                "key",
                ExpectedRevision::Absent,
                write("open", &payload),
            )
            .unwrap(),
    );
    assert_eq!(created.revision, 1);

    // A second create through another connection sees the stored record.
    match second
        .put_coordination_record(
            NAMESPACE,
            "key",
            ExpectedRevision::Absent,
            write("open", &payload),
        )
        .unwrap()
    {
        RecordOutcome::Conflict { current } => assert_eq!(current, Some(created.clone())),
        other => panic!("create over an existing record must conflict: {other:?}"),
    }

    let updated = applied(
        second
            .put_coordination_record(
                NAMESPACE,
                "key",
                ExpectedRevision::Revision(1),
                write("closed", &json!({"step": 2})),
            )
            .unwrap(),
    );
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.created_at_ms, created.created_at_ms);

    // The stale revision no longer matches either write or delete.
    assert!(matches!(
        first
            .put_coordination_record(
                NAMESPACE,
                "key",
                ExpectedRevision::Revision(1),
                write("open", &payload),
            )
            .unwrap(),
        RecordOutcome::Conflict { current: Some(ref current) } if current.revision == 2
    ));
    assert!(matches!(
        first
            .delete_coordination_record(NAMESPACE, "key", 1)
            .unwrap(),
        RecordOutcome::Conflict { .. }
    ));
    assert_eq!(
        first.coordination_record(NAMESPACE, "key").unwrap(),
        Some(updated)
    );

    assert_eq!(
        first
            .delete_coordination_record(NAMESPACE, "key", 2)
            .unwrap(),
        RecordOutcome::Applied(None)
    );
    assert_eq!(second.coordination_record(NAMESPACE, "key").unwrap(), None);
    // Namespaces are independent.
    assert_eq!(second.coordination_record("other", "key").unwrap(), None);
}

#[test]
fn an_expired_record_reads_as_absent_and_continues_its_revision() {
    let (_dir, store, _) = store_pair();
    let payload = json!({});
    let expiring = RecordWrite {
        state: "claimed",
        payload: &payload,
        expires_at_ms: Some(1_000),
    };
    put_record_at(
        &store.connection,
        NAMESPACE,
        "key",
        ExpectedRevision::Absent,
        expiring,
        500,
    )
    .unwrap();
    assert!(read_record(&store.connection, NAMESPACE, "key", 999)
        .unwrap()
        .is_some());
    assert!(read_record(&store.connection, NAMESPACE, "key", 1_000)
        .unwrap()
        .is_none());

    let replaced = applied(
        put_record_at(
            &store.connection,
            NAMESPACE,
            "key",
            ExpectedRevision::Absent,
            write("claimed", &payload),
            2_000,
        )
        .unwrap(),
    );
    assert_eq!(replaced.revision, 2, "revisions must never repeat");
    assert_eq!(replaced.created_at_ms, 2_000);
}

#[test]
fn an_exclusive_lease_admits_one_holder_across_connections() {
    let (_dir, first, second) = store_pair();
    let empty = json!({});

    let holder = acquired(
        first
            .acquire_coordination_lease(exclusive("key", LeaseOwner::current_process(), &empty))
            .unwrap(),
    );
    match second
        .acquire_coordination_lease(exclusive("key", LeaseOwner::current_process(), &empty))
        .unwrap()
    {
        LeaseAcquire::Contended(holders) => assert_eq!(holders, vec![holder.clone()]),
        other => panic!("second exclusive acquire must contend: {other:?}"),
    }

    assert!(matches!(
        second
            .renew_coordination_lease(NAMESPACE, "key", &holder.holder_token, holder.fence, TTL)
            .unwrap(),
        LeaseRenew::Renewed(_)
    ));
    assert_eq!(
        second
            .renew_coordination_lease(
                NAMESPACE,
                "key",
                &holder.holder_token,
                holder.fence + 1,
                TTL
            )
            .unwrap(),
        LeaseRenew::Lost,
        "a renew with the wrong fence must not extend the lease"
    );
    assert!(first
        .release_coordination_lease(NAMESPACE, "key", &holder.holder_token)
        .unwrap());
    acquired(
        second
            .acquire_coordination_lease(exclusive("key", LeaseOwner::current_process(), &empty))
            .unwrap(),
    );
}

#[test]
fn concurrent_exclusive_acquirers_elect_exactly_one_holder() {
    const CONTENDERS: usize = 6;
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("observations.sqlite");
    drop(ObservationStore::open_initialized_for_lifecycle_at(&path).unwrap());
    let barrier = Arc::new(Barrier::new(CONTENDERS));
    let threads: Vec<_> = (0..CONTENDERS)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let store = ObservationStore::open_initialized_for_lifecycle_at(&path).unwrap();
                let empty = json!({});
                barrier.wait();
                matches!(
                    store
                        .acquire_coordination_lease(exclusive(
                            "key",
                            LeaseOwner::current_process(),
                            &empty
                        ))
                        .unwrap(),
                    LeaseAcquire::Acquired(_)
                )
            })
        })
        .collect();
    let winners = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);
}

#[test]
fn a_capacity_semaphore_admits_until_the_weights_reach_capacity() {
    let (_dir, first, second) = store_pair();
    let empty = json!({});
    let fits = |holders: &[LeaseHolder]| {
        let used: u64 = holders
            .iter()
            .map(|holder| holder.weight["slots"].as_u64().unwrap_or(0))
            .sum();
        used + 2 <= 5
    };
    let weight = json!({"slots": 2});
    let request = |weight| LeaseRequest {
        namespace: NAMESPACE,
        key: "pool",
        ttl: TTL,
        owner: LeaseOwner::current_process(),
        admission: LeaseAdmission::Capacity(&fits),
        weight,
        payload: &empty,
    };

    let a = acquired(first.acquire_coordination_lease(request(&weight)).unwrap());
    let b = acquired(second.acquire_coordination_lease(request(&weight)).unwrap());
    assert!(b.fence > a.fence);
    match first.acquire_coordination_lease(request(&weight)).unwrap() {
        LeaseAcquire::Contended(holders) => assert_eq!(holders.len(), 2),
        other => panic!("a third weight-2 holder exceeds capacity 5: {other:?}"),
    }
    assert!(second
        .release_coordination_lease(NAMESPACE, "pool", &a.holder_token)
        .unwrap());
    acquired(first.acquire_coordination_lease(request(&weight)).unwrap());
    assert_eq!(
        first
            .coordination_lease_holders(NAMESPACE, "pool")
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn the_fence_stays_monotonic_across_release_and_reacquire() {
    let (_dir, first, second) = store_pair();
    let empty = json!({});
    let mut last = 0;
    for round in 0..4 {
        let store = if round % 2 == 0 { &first } else { &second };
        let holder = acquired(
            store
                .acquire_coordination_lease(exclusive("key", LeaseOwner::current_process(), &empty))
                .unwrap(),
        );
        assert!(holder.fence > last, "fence {} after {last}", holder.fence);
        last = holder.fence;
        assert!(store
            .release_coordination_lease(NAMESPACE, "key", &holder.holder_token)
            .unwrap());
        // A released holder can no longer renew, even with its own fence.
        assert_eq!(
            store
                .renew_coordination_lease(NAMESPACE, "key", &holder.holder_token, holder.fence, TTL)
                .unwrap(),
            LeaseRenew::Lost
        );
    }
    let fence: i64 = first
        .connection
        .query_row(
            "SELECT last_fence FROM coordination_fences WHERE namespace = ?1 AND lease_key = 'key'",
            [NAMESPACE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fence, last);
}

#[test]
fn an_expired_holder_cannot_renew_and_is_replaced_by_the_next_acquire() {
    let (_dir, store, _) = store_pair();
    let empty = json!({});
    let stale = acquired(
        acquire_lease_at(
            &store.connection,
            exclusive("key", LeaseOwner::current_process(), &empty),
            1_000,
        )
        .unwrap(),
    );
    let expired_at = stale.expires_at_ms;
    assert_eq!(
        renew_lease_at(
            &store.connection,
            NAMESPACE,
            "key",
            &stale.holder_token,
            stale.fence,
            TTL,
            expired_at,
        )
        .unwrap(),
        LeaseRenew::Lost
    );
    let next = acquired(
        acquire_lease_at(
            &store.connection,
            exclusive("key", LeaseOwner::current_process(), &empty),
            expired_at,
        )
        .unwrap(),
    );
    assert!(next.fence > stale.fence);
}

/// Every holder here is judged at a fixed clock with fabricated owners, so the
/// outcome does not depend on wall-clock timing or on another process exiting.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn reap_keeps_live_holders_and_removes_expired_or_dead_ones() {
    let (_dir, store, _) = store_pair();
    let empty = json!({});
    let now = 10_000;
    let this_host = Some("this-host".to_string());
    let live = LeaseOwner {
        host: this_host.clone(),
        ..LeaseOwner::current_process()
    };
    assert!(
        live.start_identity.is_some(),
        "the current process must expose a start identity on this platform"
    );
    // The current PID with a different start identity is a reused PID: the
    // recorded owner is gone even though the PID answers.
    let reused_pid = LeaseOwner {
        start_identity: Some(match live.start_identity.clone().unwrap() {
            ProcessStartIdentity::Linux { starttime_ticks } => ProcessStartIdentity::Linux {
                starttime_ticks: starttime_ticks.wrapping_add(1),
            },
            ProcessStartIdentity::Macos {
                start_seconds,
                start_microseconds,
            } => ProcessStartIdentity::Macos {
                start_seconds: start_seconds.wrapping_add(1),
                start_microseconds,
            },
        }),
        ..live.clone()
    };
    // Same apparent owner on another host: local process evidence says
    // nothing about it, so only expiry can remove it.
    let other_host = LeaseOwner {
        host: Some("other-host".to_string()),
        ..reused_pid.clone()
    };
    let no_pid = LeaseOwner {
        pid: None,
        host: this_host.clone(),
        start_identity: None,
    };

    let admit_all = |key: &'static str, owner: LeaseOwner, at: i64| {
        let accept = |_: &[LeaseHolder]| true;
        acquired(
            acquire_lease_at(
                &store.connection,
                LeaseRequest {
                    namespace: NAMESPACE,
                    key,
                    ttl: Duration::from_millis(1_000),
                    owner,
                    admission: LeaseAdmission::Capacity(&accept),
                    weight: &empty,
                    payload: &empty,
                },
                at,
            )
            .unwrap(),
        )
    };
    let kept_live = admit_all("a", live.clone(), now);
    let kept_remote = admit_all("a", other_host, now);
    let kept_unverifiable = admit_all("b", no_pid, now);
    let dead = admit_all("b", reused_pid, now);
    // Acquired long enough ago that it expired before `now`, though its owner
    // is the live current process.
    let expired = admit_all("c", live, now - 5_000);

    let mut reaped: Vec<String> = reap_leases_at(&store.connection, NAMESPACE, now, &this_host)
        .unwrap()
        .into_iter()
        .map(|holder| holder.holder_token)
        .collect();
    reaped.sort();
    let mut expected = vec![dead.holder_token, expired.holder_token];
    expected.sort();
    assert_eq!(reaped, expected);

    let mut remaining: Vec<String> = holders_on(&store.connection, NAMESPACE, None)
        .unwrap()
        .into_iter()
        .map(|holder| holder.holder_token)
        .collect();
    remaining.sort();
    let mut kept = vec![
        kept_live.holder_token,
        kept_remote.holder_token,
        kept_unverifiable.holder_token,
    ];
    kept.sort();
    assert_eq!(remaining, kept);

    // A second reap at the same instant is a no-op.
    assert!(
        reap_leases_at(&store.connection, NAMESPACE, now, &this_host)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn audit_appends_and_reads_a_bounded_newest_first_window_per_subject() {
    let (_dir, first, second) = store_pair();
    for index in 0..5 {
        first
            .append_coordination_audit(NAMESPACE, "subject", "event", &json!({"index": index}))
            .unwrap();
    }
    second
        .append_coordination_audit(NAMESPACE, "other", "event", &json!({}))
        .unwrap();

    let window = second.coordination_audit(NAMESPACE, "subject", 3).unwrap();
    let indexes: Vec<_> = window
        .iter()
        .map(|entry| entry.payload["index"].as_i64().unwrap())
        .collect();
    assert_eq!(indexes, vec![4, 3, 2]);
    assert!(window
        .windows(2)
        .all(|pair| pair[0].sequence > pair[1].sequence));
    assert_eq!(
        second
            .coordination_audit(NAMESPACE, "subject", usize::MAX)
            .unwrap()
            .len(),
        5
    );
}
