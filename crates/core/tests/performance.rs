//! PF-1 / PF-2: 200,000 users searched within 10 ms, 5,000 objects updated within a 30 fps frame.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use envmon_core::Level;
use envmon_core::demo::{self, ROOM_DOOR};
use envmon_core::model::ObjectId;
use envmon_core::objects::{MAX_OBJECTS, ObjectKind, ObjectRegistry, ObjectValue};
use envmon_core::store::Store;

const USERS: u32 = 200_000;
const SEARCH_LIMIT: Duration = Duration::from_millis(10);
const FRAME_BUDGET: Duration = Duration::from_micros(33_333);

#[test]
fn authenticate_among_200k_users_within_10ms() {
    let t = Instant::now();
    let site = demo::site(USERS);
    let mut store = Store::open_in_memory().unwrap();
    demo::save(&site.directory, &mut store).unwrap();
    let registered = t.elapsed();

    let t = Instant::now();
    let directory = store.load_directory().unwrap();
    let loaded = t.elapsed();
    assert_eq!(directory.len(), USERS as usize);

    let at = demo::epoch() + chrono::TimeDelta::days(30);
    let mut worst = Duration::ZERO;
    // Spread over the whole ID range, plus unknown cards.
    for n in (1..=USERS)
        .step_by(97)
        .chain([USERS, USERS + 1, u32::MAX >> 8])
    {
        let card = demo::card(n);
        let t = Instant::now();
        let decision = directory.authorize_door(&card, ROOM_DOOR, at);
        worst = worst.max(t.elapsed());
        assert_eq!(decision.is_granted(), n <= USERS, "user {n}");
    }
    println!("register {registered:?}, load {loaded:?}, worst search {worst:?}");
    assert!(worst < SEARCH_LIMIT, "worst search took {worst:?}");
}

#[test]
fn update_5000_objects_within_one_30fps_frame() {
    let mut reg = ObjectRegistry::new();
    for i in 0..MAX_OBJECTS as u32 {
        reg.register(ObjectId(i), ObjectKind::AnalogSensor, format!("s{i}"))
            .unwrap();
    }
    let mut worst = Duration::ZERO;
    for frame in 0..30 {
        let t = Instant::now();
        let v = reg.version();
        for i in 0..MAX_OBJECTS as u32 {
            let value = ObjectValue::Analog {
                value: (frame * 7 + i) as f32,
                level: Level::Normal,
            };
            reg.update(ObjectId(i), value).unwrap();
        }
        assert_eq!(reg.changed_since(v).count(), MAX_OBJECTS);
        worst = worst.max(t.elapsed());
    }
    println!("worst frame update {worst:?}");
    assert!(worst < FRAME_BUDGET, "worst frame took {worst:?}");
}
