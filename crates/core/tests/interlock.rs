//! IL-1 / HW-5: the rack door and the key cabinet open only after entering the server room.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use chrono::TimeDelta;
use envmon_core::demo::{self, KEY_CABINET, RACK_DOOR, RACK_READER, ROOM_READER};
use envmon_core::directory::{AccessDecision, DenyReason};
use envmon_core::model::UserId;
use envmon_core::rules::RuleId;
use envmon_core::store::Store;
use envmon_core::system::{AccessPoint, ControlCommand, System};

fn system() -> System {
    let site = demo::site(10);
    System::new(site.directory, site.rules, site.doors)
        .unwrap()
        .with_store(Store::open_in_memory().unwrap())
        .unwrap()
}

fn interlock(rule: u32) -> DenyReason {
    let name = if rule == 1 {
        "rack requires server room entry"
    } else {
        "key cabinet requires server room entry"
    };
    DenyReason::Interlock {
        rule: RuleId(rule),
        name: name.to_owned(),
    }
}

#[test]
fn rack_and_key_cabinet_stay_closed_without_room_entry() {
    let mut sys = system();
    let at = demo::epoch() + TimeDelta::days(1);
    let card = demo::card(3);

    let rack = sys
        .card_presented(RACK_READER, card.clone(), at, Instant::now())
        .unwrap();
    assert_eq!(
        rack,
        AccessDecision::Denied {
            user: Some(UserId(3)),
            reason: interlock(1)
        }
    );
    let key = sys.key_requested(KEY_CABINET, card, at).unwrap();
    assert_eq!(
        key,
        AccessDecision::Denied {
            user: Some(UserId(3)),
            reason: interlock(2)
        }
    );
    assert_eq!(
        sys.take_commands(),
        vec![ControlCommand::ReaderFeedback {
            reader: RACK_READER,
            granted: false
        }]
    );
    let last = sys.history().next_back().unwrap();
    assert_eq!(
        (last.point, last.granted),
        (AccessPoint::KeyCabinet(KEY_CABINET), false)
    );
}

#[test]
fn rack_and_key_cabinet_open_after_room_entry_within_window() {
    let mut sys = system();
    let at = demo::epoch() + TimeDelta::days(1);
    let card = demo::card(3);

    assert!(
        sys.card_presented(ROOM_READER, card.clone(), at, Instant::now())
            .unwrap()
            .is_granted()
    );
    sys.take_commands();

    let later = at + TimeDelta::minutes(30);
    assert!(
        sys.card_presented(RACK_READER, card.clone(), later, Instant::now())
            .unwrap()
            .is_granted()
    );
    assert!(
        sys.key_requested(KEY_CABINET, card.clone(), later)
            .unwrap()
            .is_granted()
    );
    let commands = sys.take_commands();
    assert!(commands.contains(&ControlCommand::Unlock {
        door: RACK_DOOR,
        duration_ms: 5_000
    }));
    assert!(commands.contains(&ControlCommand::ReleaseKey {
        cabinet: KEY_CABINET
    }));

    // Another user who has not entered the room is still refused.
    assert!(
        !sys.key_requested(KEY_CABINET, demo::card(4), later)
            .unwrap()
            .is_granted()
    );

    // The entry expires after the configured window (1 h).
    let expired = at + TimeDelta::minutes(61);
    assert!(
        !sys.key_requested(KEY_CABINET, card, expired)
            .unwrap()
            .is_granted()
    );
}

#[test]
fn access_history_survives_restart() {
    let path = std::env::temp_dir().join(format!("envmon-history-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let open = || {
        let site = demo::site(10);
        System::new(site.directory, site.rules, site.doors)
            .unwrap()
            .with_store(Store::open(&path).unwrap())
            .unwrap()
    };
    let mut sys = open();
    let at = demo::epoch() + chrono::TimeDelta::days(1);
    sys.card_presented(demo::ROOM_READER, demo::card(1), at, std::time::Instant::now())
        .unwrap();
    drop(sys);
    let sys = open();
    let restored: Vec<_> = sys.history().collect();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].point, AccessPoint::Door(demo::ROOM_DOOR));
    drop(sys);
    let _ = std::fs::remove_file(&path);
}
