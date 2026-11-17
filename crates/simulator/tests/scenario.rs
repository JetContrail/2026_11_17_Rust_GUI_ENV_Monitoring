//! End-to-end: virtual devices ↔ encrypted link ↔ host ↔ core system.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use envmon_core::Level;
use envmon_core::demo::{self, KEY_CABINET, RACK_DOOR, RACK_READER, ROOM_DOOR, ROOM_READER};
use envmon_core::door::{AlarmKind, DoorState, LockState};
use envmon_core::objects::ObjectValue;
use envmon_core::system::ControlCommand;
use envmon_protocol::device::KeyCabinet;
use envmon_protocol::message::Downlink;
use envmon_simulator::{
    KEY_CABINET_OBJECT, RACK_OUTLET, Scenario, SimClock, StepReport, TEMPERATURE, Transport,
    VirtualSite, demo_scenario, demo_system, run_scenario,
};

async fn run(
    transport: Transport,
) -> (
    Vec<StepReport>,
    Arc<Mutex<envmon_core::system::System>>,
    VirtualSite,
) {
    let clock = SimClock::new(demo::epoch() + chrono::TimeDelta::days(1));
    let system = Arc::new(Mutex::new(demo_system(100).unwrap()));
    let mut site = VirtualSite::demo(Instant::now());
    let reports = run_scenario(
        Arc::clone(&system),
        &mut site,
        &clock,
        &demo_scenario(),
        transport,
    )
    .await
    .unwrap();
    (reports, system, site)
}

fn door(r: &StepReport, id: envmon_core::model::DoorId) -> envmon_core::door::DoorIndication {
    r.doors.iter().find(|(d, _)| *d == id).unwrap().1
}

fn feedback(reader: envmon_core::model::ReaderId, granted: bool) -> Downlink {
    Downlink::Control(ControlCommand::ReaderFeedback { reader, granted })
}

async fn check_demo(transport: Transport) {
    let (r, system, site) = run(transport).await;
    assert_eq!(r.len(), demo_scenario().steps.len());

    // Rack and key cabinet refused before entering the room.
    assert_eq!(r[0].downlinks, vec![feedback(RACK_READER, false)]);
    assert!(r[1].downlinks.is_empty());

    // Room entry unlocks the room door.
    assert_eq!(
        r[2].downlinks,
        vec![
            Downlink::Control(ControlCommand::Unlock {
                door: ROOM_DOOR,
                duration_ms: 5000
            }),
            feedback(ROOM_READER, true),
        ]
    );
    assert_eq!(door(&r[2], ROOM_DOOR).lock, LockState::Unlocked);

    // Door opened while unlocked: no alarm; 5 s later still open, re-locked, no alarm.
    assert_eq!(door(&r[3], ROOM_DOOR).door, DoorState::Open);
    assert_eq!(
        (door(&r[4], ROOM_DOOR).door, door(&r[4], ROOM_DOOR).lock),
        (DoorState::Open, LockState::Locked)
    );
    assert_eq!(r[4].active_alarms, 0);

    // 11 s open (> 10 s): open-too-long alarm with sound and red blinking.
    let ind = door(&r[5], ROOM_DOOR);
    assert_eq!(ind.door, DoorState::OpenTooLong);
    assert!(ind.sound && ind.blink);
    assert_eq!(r[5].active_alarms, 1);

    // Closing clears it.
    assert_eq!(door(&r[9], ROOM_DOOR).door, DoorState::Closed);
    assert_eq!(r[9].active_alarms, 0);

    // After entry the rack door and key cabinet open.
    assert_eq!(
        r[10].downlinks,
        vec![
            Downlink::Control(ControlCommand::Unlock {
                door: RACK_DOOR,
                duration_ms: 5000
            }),
            feedback(RACK_READER, true),
        ]
    );
    assert_eq!(
        r[11].downlinks,
        vec![Downlink::Control(ControlCommand::ReleaseKey {
            cabinet: KEY_CABINET
        })]
    );
    assert_eq!(site.key_cabinets[&KEY_CABINET].1.keys_out(), 1);
    assert_eq!(site.readers[&RACK_READER].last_feedback, Some(true));

    let sys = system.lock().unwrap();
    assert_eq!(
        sys.objects().get(TEMPERATURE).unwrap().value,
        ObjectValue::Analog {
            value: 41.0,
            level: Level::Alarm
        }
    );
    assert_eq!(
        sys.objects().get(KEY_CABINET_OBJECT).unwrap().value,
        ObjectValue::KeyCabinet { keys_out: 1 }
    );
    assert_eq!(
        sys.objects().get(RACK_OUTLET).unwrap().value,
        ObjectValue::Outlet {
            on: true,
            watts: 420.0
        }
    );
    let alarm = sys.alarms().next().unwrap();
    assert_eq!(
        (alarm.door, alarm.kind),
        (ROOM_DOOR, AlarmKind::OpenTooLong)
    );
    assert!(alarm.cleared_at.is_some());
    let granted = sys.history().filter(|h| h.granted).count();
    let denied = sys.history().filter(|h| !h.granted).count();
    assert_eq!((granted, denied), (3, 2));
}

#[tokio::test]
async fn demo_scenario_over_mutual_tls() {
    check_demo(Transport::Tls).await;
}

#[tokio::test]
async fn demo_scenario_over_aes_gcm() {
    check_demo(Transport::Aes).await;
}

#[test]
fn scenarios_load_from_json() {
    let json = serde_json::to_string(&demo_scenario()).unwrap();
    assert!(json.contains(r#""step":"card_touch""#));
    let parsed: Scenario = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, demo_scenario());
    let custom: Scenario = serde_json::from_str(
        r#"{"name":"x","steps":[{"step":"open_door","door":1},{"step":"wait","secs":12}]}"#,
    )
    .unwrap();
    assert_eq!(custom.steps.len(), 2);
}

#[tokio::test]
async fn scenario_inputs_unknown_to_the_site_are_in_scope() {
    let door_contact = envmon_core::model::ObjectId(400);
    let mut sys = demo_system(10).unwrap();
    sys.objects_mut()
        .register(
            door_contact,
            envmon_core::objects::ObjectKind::DigitalInput,
            "Rack door contact",
        )
        .unwrap();
    let system = Arc::new(Mutex::new(sys));
    let scenario = Scenario {
        name: "contact".into(),
        steps: vec![envmon_simulator::Step::DigitalInput {
            object: door_contact,
            on: true,
        }],
    };
    let clock = SimClock::new(demo::epoch());
    let mut site = VirtualSite::demo(Instant::now());
    run_scenario(
        Arc::clone(&system),
        &mut site,
        &clock,
        &scenario,
        Transport::Aes,
    )
    .await
    .unwrap();
    assert_eq!(
        system
            .lock()
            .unwrap()
            .objects()
            .get(door_contact)
            .unwrap()
            .value,
        ObjectValue::Digital { on: true }
    );
}
