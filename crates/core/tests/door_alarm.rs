//! AL-1 / ST-1 through the System facade: open-too-long and forced-open alarms.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use chrono::TimeDelta;
use envmon_core::demo::{self, ROOM_DOOR, ROOM_READER};
use envmon_core::door::{AlarmKind, DoorState, LockState};
use envmon_core::model::ObjectId;
use envmon_core::objects::ObjectValue;
use envmon_core::system::{ControlCommand, System};

fn system() -> System {
    let site = demo::site(10);
    System::new(site.directory, site.rules, site.doors).unwrap()
}

#[test]
fn door_left_open_raises_alarm_until_acknowledged_and_closed() {
    let mut sys = system();
    let t0 = Instant::now();
    let at0 = demo::epoch() + TimeDelta::days(1);
    let at = |s: i64| at0 + TimeDelta::seconds(s);
    let now = |s: i64| t0 + Duration::from_secs(s.unsigned_abs());

    assert!(
        sys.card_presented(ROOM_READER, demo::card(1), at(0), now(0))
            .unwrap()
            .is_granted()
    );
    assert_eq!(
        sys.take_commands(),
        vec![
            ControlCommand::Unlock {
                door: ROOM_DOOR,
                duration_ms: 5_000
            },
            ControlCommand::ReaderFeedback {
                reader: ROOM_READER,
                granted: true
            },
        ]
    );
    assert_eq!(
        sys.door_indication(ROOM_DOOR).unwrap().lock,
        LockState::Unlocked
    );

    sys.door_sensor(ROOM_DOOR, true, at(1), now(1)).unwrap();
    for s in 2..=10 {
        sys.tick(at(s), now(s)).unwrap();
    }
    let ind = sys.door_indication(ROOM_DOOR).unwrap();
    assert_eq!(
        (ind.door, ind.lock, ind.sound),
        (DoorState::Open, LockState::Locked, false)
    );
    assert_eq!(sys.active_alarms().count(), 0);

    sys.tick(at(11), now(11)).unwrap();
    let ind = sys.door_indication(ROOM_DOOR).unwrap();
    assert_eq!(ind.door, DoorState::OpenTooLong);
    assert!(ind.sound && ind.blink);
    let alarm = sys.active_alarms().next().unwrap().clone();
    assert_eq!(
        (alarm.door, alarm.kind, alarm.raised_at),
        (ROOM_DOOR, AlarmKind::OpenTooLong, at(11))
    );
    // The screen object follows the door.
    assert_eq!(
        sys.objects().get(ObjectId(1)).unwrap().value,
        ObjectValue::Door(ind)
    );

    assert!(sys.acknowledge(ROOM_DOOR, at(20)).unwrap());
    let ind = sys.door_indication(ROOM_DOOR).unwrap();
    assert_eq!(ind.door, DoorState::OpenAcknowledged);
    assert!(!ind.sound && !ind.blink);
    assert_eq!(
        sys.active_alarms().next().unwrap().acknowledged_at,
        Some(at(20))
    );

    sys.door_sensor(ROOM_DOOR, false, at(30), now(30)).unwrap();
    assert_eq!(
        sys.door_indication(ROOM_DOOR).unwrap().door,
        DoorState::Closed
    );
    assert_eq!(sys.active_alarms().count(), 0);
    assert_eq!(sys.alarms().next().unwrap().cleared_at, Some(at(30)));
}

#[test]
fn opening_without_unlock_is_forced_open() {
    let mut sys = system();
    let t0 = Instant::now();
    let at = demo::epoch();
    sys.door_sensor(ROOM_DOOR, true, at, t0).unwrap();
    let ind = sys.door_indication(ROOM_DOOR).unwrap();
    assert_eq!(ind.door, DoorState::ForcedOpen);
    assert!(ind.sound && ind.blink);
    assert_eq!(
        sys.active_alarms().next().unwrap().kind,
        AlarmKind::ForcedOpen
    );
}

#[test]
fn denied_card_does_not_unlock() {
    let mut sys = system();
    let d = sys
        .card_presented(ROOM_READER, demo::card(999), demo::epoch(), Instant::now())
        .unwrap();
    assert!(!d.is_granted());
    assert_eq!(
        sys.take_commands(),
        vec![ControlCommand::ReaderFeedback {
            reader: ROOM_READER,
            granted: false
        }]
    );
    assert_eq!(
        sys.door_indication(ROOM_DOOR).unwrap().lock,
        LockState::Locked
    );
}
