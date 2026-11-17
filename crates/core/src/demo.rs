//! Demo / test site: a server room with a rack and a key cabinet that open only after
//! entering the room. Used by tests, benchmarks and the simulator.

use std::time::Duration;

use chrono::{NaiveDate, NaiveDateTime};

use crate::directory::UserDirectory;
use crate::model::{
    AccessGroup, AreaId, CardId, DoorConfig, DoorId, DoorPermission, GroupId, KeyCabinetId,
    ObjectId, ReaderId, Schedule, ScheduleId, User, UserId,
};
use crate::rules::{Condition, Rule, RuleEngine, RuleId, Target};
use crate::store::{Store, StoreError};

pub const ROOM_DOOR: DoorId = DoorId(1);
pub const RACK_DOOR: DoorId = DoorId(2);
pub const ROOM_READER: ReaderId = ReaderId(1);
pub const RACK_READER: ReaderId = ReaderId(2);
pub const SERVER_ROOM: AreaId = AreaId(1);
pub const KEY_CABINET: KeyCabinetId = KeyCabinetId(1);
pub const STAFF: GroupId = GroupId(1);
/// Interlock window: rack / key cabinet open only within this time after entering the room.
pub const ENTRY_WINDOW: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub struct Site {
    pub directory: UserDirectory,
    pub rules: RuleEngine,
    pub doors: Vec<DoorConfig>,
}

/// 2026-01-01 00:00, the start of every demo user's validity.
pub fn epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap_or_default()
}

/// Card of demo user `n` (8-byte FeliCa-IDm-like ID).
pub fn card(n: u32) -> CardId {
    let bytes = (0x0123_0000_0000_0000_u64 | u64::from(n)).to_be_bytes();
    CardId::from_bytes(&bytes).unwrap_or_else(|_| unreachable!("8 bytes is a valid card ID"))
}

/// Users `1..=users`, all members of [`STAFF`].
pub fn site(users: u32) -> Site {
    let mut directory = UserDirectory::with_capacity(users as usize);
    directory.insert_schedule(Schedule::always(ScheduleId(0)));
    let staff = AccessGroup {
        id: STAFF,
        name: "staff".to_owned(),
        doors: [ROOM_DOOR, RACK_DOOR]
            .into_iter()
            .map(|door| DoorPermission {
                door,
                schedule: ScheduleId(0),
            })
            .collect(),
        key_cabinets: vec![KEY_CABINET],
    };
    if let Err(e) = directory.insert_group(staff) {
        unreachable!("demo group is consistent: {e}");
    }
    for n in 1..=users {
        let user = User {
            id: UserId(n),
            name: format!("user{n:06}"),
            cards: vec![card(n)],
            groups: vec![STAFF],
            valid_from: epoch(),
            valid_until: None,
        };
        if let Err(e) = directory.upsert_user(user) {
            unreachable!("demo users are consistent: {e}");
        }
    }

    let entered_room = Condition::EnteredArea {
        area: SERVER_ROOM,
        within_secs: Some(ENTRY_WINDOW.as_secs()),
    };
    let rules = RuleEngine::new([
        Rule {
            id: RuleId(1),
            name: "rack requires server room entry".to_owned(),
            target: Target::Door(RACK_DOOR),
            condition: entered_room.clone(),
        },
        Rule {
            id: RuleId(2),
            name: "key cabinet requires server room entry".to_owned(),
            target: Target::KeyCabinet(KEY_CABINET),
            condition: entered_room,
        },
    ]);

    let mut room = DoorConfig::new(ROOM_DOOR, "server room");
    room.readers = vec![ROOM_READER];
    room.enters_area = Some(SERVER_ROOM);
    room.object = Some(ObjectId(1));
    let mut rack = DoorConfig::new(RACK_DOOR, "rack A01");
    rack.readers = vec![RACK_READER];
    rack.object = Some(ObjectId(2));

    Site {
        directory,
        rules,
        doors: vec![room, rack],
    }
}

/// Writes the master data of `directory` to `store`.
pub fn save(directory: &UserDirectory, store: &mut Store) -> Result<(), StoreError> {
    for s in directory.schedules() {
        store.save_schedule(s)?;
    }
    for g in directory.groups() {
        store.save_group(g)?;
    }
    let mut users: Vec<User> = directory.users().cloned().collect();
    users.sort_by_key(|u| u.id);
    store.save_users(&users)
}
