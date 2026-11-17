//! Identifiers and master data shared by every crate.

use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(UserId);
id_type!(DoorId);
id_type!(ReaderId);
id_type!(AreaId);
id_type!(GroupId);
id_type!(ScheduleId);
id_type!(KeyCabinetId);
id_type!(
    /// A monitored or controlled object on the monitoring screen (door, sensor, PDU outlet, ...).
    ObjectId
);

/// Maximum length of a card ID in bytes (Mifare triple-size UID is 10 bytes).
pub const MAX_CARD_ID_LEN: usize = 16;

/// Card identifier read by a card reader (FeliCa IDm, Mifare UID, Type B PUPI, ...).
///
/// Displayed and serialized as upper-case hexadecimal.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CardId(Box<[u8]>);

/// Error returned for an empty, too long or non-hexadecimal card ID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid card ID: {0}")]
pub struct InvalidCardId(pub String);

impl CardId {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InvalidCardId> {
        if bytes.is_empty() || bytes.len() > MAX_CARD_ID_LEN {
            return Err(InvalidCardId(format!("{} bytes", bytes.len())));
        }
        Ok(Self(bytes.into()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02X}"))
    }
}

impl fmt::Debug for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CardId({self})")
    }
}

impl FromStr for CardId {
    type Err = InvalidCardId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidCardId(s.to_owned());
        if !s.len().is_multiple_of(2) || !s.is_ascii() {
            return Err(invalid());
        }
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| invalid()))
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_bytes(&bytes).map_err(|_| invalid())
    }
}

impl Serialize for CardId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CardId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A registered card holder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub name: String,
    pub cards: Vec<CardId>,
    pub groups: Vec<GroupId>,
    /// First moment the user may pass (inclusive).
    pub valid_from: NaiveDateTime,
    /// End of validity (exclusive); `None` means no expiry.
    pub valid_until: Option<NaiveDateTime>,
}

impl User {
    pub fn is_valid_at(&self, at: NaiveDateTime) -> Validity {
        if at < self.valid_from {
            Validity::NotYetValid
        } else if self.valid_until.is_some_and(|until| at >= until) {
            Validity::Expired
        } else {
            Validity::Valid
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Valid,
    NotYetValid,
    Expired,
}

/// Door and key-cabinet permissions shared by many users.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessGroup {
    pub id: GroupId,
    pub name: String,
    pub doors: Vec<DoorPermission>,
    pub key_cabinets: Vec<KeyCabinetId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorPermission {
    pub door: DoorId,
    pub schedule: ScheduleId,
}

/// Half-open time range `[start, end)` within a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl TimeRange {
    pub fn contains(&self, t: NaiveTime) -> bool {
        self.start <= t && t < self.end
    }
}

/// Weekly schedule: allowed time ranges per weekday (index 0 = Monday).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: ScheduleId,
    pub name: String,
    pub weekly: [Vec<TimeRange>; 7],
}

impl Schedule {
    /// A schedule that allows every moment of every day.
    pub fn always(id: ScheduleId) -> Self {
        let all_day = vec![TimeRange {
            start: NaiveTime::MIN,
            end: NaiveTime::from_hms_nano_opt(23, 59, 59, 1_999_999_999).unwrap_or(NaiveTime::MIN),
        }];
        Self {
            id,
            name: "always".to_owned(),
            weekly: std::array::from_fn(|_| all_day.clone()),
        }
    }

    pub fn allows(&self, at: NaiveDateTime) -> bool {
        let day = at.weekday().num_days_from_monday() as usize;
        self.weekly[day].iter().any(|r| r.contains(at.time()))
    }
}

/// Static configuration of a door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorConfig {
    pub id: DoorId,
    pub name: String,
    /// Readers that request access through this door.
    pub readers: Vec<ReaderId>,
    /// Area the card holder is in after passing the door.
    pub enters_area: Option<AreaId>,
    /// Monitoring-screen object that displays this door.
    pub object: Option<ObjectId>,
    #[serde(with = "duration_ms")]
    pub unlock_duration: std::time::Duration,
    #[serde(with = "duration_ms")]
    pub open_alarm_after: std::time::Duration,
}

/// Default time the lock stays released after an unlock command (5 s).
pub const DEFAULT_UNLOCK_DURATION: std::time::Duration = std::time::Duration::from_secs(5);
/// Default time a door may stay open before the open-too-long alarm (10 s).
pub const DEFAULT_OPEN_ALARM_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

impl DoorConfig {
    pub fn new(id: DoorId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            readers: Vec::new(),
            enters_area: None,
            object: None,
            unlock_duration: DEFAULT_UNLOCK_DURATION,
            open_alarm_after: DEFAULT_OPEN_ALARM_AFTER,
        }
    }
}

mod duration_ms {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn card_id_round_trips_as_hex() {
        let id: CardId = "0123456789abcdef".parse().unwrap();
        assert_eq!(id.to_string(), "0123456789ABCDEF");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"0123456789ABCDEF\"");
        assert!("".parse::<CardId>().is_err());
        assert!("ABC".parse::<CardId>().is_err());
        assert!("ZZ".parse::<CardId>().is_err());
        assert!("00".repeat(MAX_CARD_ID_LEN + 1).parse::<CardId>().is_err());
    }

    #[test]
    fn schedule_checks_weekday_and_half_open_range() {
        let mut s = Schedule::always(ScheduleId(1));
        let office = vec![TimeRange {
            start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
        }];
        s.weekly = std::array::from_fn(|i| if i < 5 { office.clone() } else { vec![] });
        // 2026-11-17 is a Tuesday, 2026-11-21 a Saturday.
        assert!(s.allows(at(17, 9, 0)));
        assert!(!s.allows(at(17, 18, 0)));
        assert!(!s.allows(at(21, 12, 0)));
        assert!(Schedule::always(ScheduleId(0)).allows(at(21, 23, 59)));
    }

    #[test]
    fn validity_period_is_half_open() {
        let user = User {
            id: UserId(1),
            name: "a".into(),
            cards: vec![],
            groups: vec![],
            valid_from: at(17, 0, 0),
            valid_until: Some(at(18, 0, 0)),
        };
        assert_eq!(user.is_valid_at(at(16, 23, 59)), Validity::NotYetValid);
        assert_eq!(user.is_valid_at(at(17, 0, 0)), Validity::Valid);
        assert_eq!(user.is_valid_at(at(18, 0, 0)), Validity::Expired);
    }
}
