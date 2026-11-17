//! In-memory user directory: O(1) card lookup for up to 200,000 users.

use std::collections::HashMap;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::model::{
    AccessGroup, CardId, DoorId, GroupId, KeyCabinetId, Schedule, ScheduleId, User, UserId,
    Validity,
};
use crate::rules::RuleId;

/// Why an access request was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DenyReason {
    UnknownCard,
    NotYetValid,
    Expired,
    NoPermission,
    OutsideSchedule,
    /// An interlock rule (e.g. "must have entered the server room") is not satisfied.
    Interlock {
        rule: RuleId,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum AccessDecision {
    Granted {
        user: UserId,
    },
    Denied {
        user: Option<UserId>,
        #[serde(flatten)]
        reason: DenyReason,
    },
}

impl AccessDecision {
    pub fn is_granted(&self) -> bool {
        matches!(self, Self::Granted { .. })
    }

    pub fn user(&self) -> Option<UserId> {
        match self {
            Self::Granted { user } => Some(*user),
            Self::Denied { user, .. } => *user,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("card {card} is already registered to user {owner}")]
    DuplicateCard { card: CardId, owner: UserId },
    #[error("unknown access group {0}")]
    UnknownGroup(GroupId),
    #[error("unknown schedule {0}")]
    UnknownSchedule(ScheduleId),
}

#[derive(Debug, Default, Clone)]
pub struct UserDirectory {
    users: HashMap<UserId, User>,
    by_card: HashMap<CardId, UserId>,
    groups: HashMap<GroupId, AccessGroup>,
    /// (group, door) -> schedules allowing the door for members of the group.
    door_index: HashMap<(GroupId, DoorId), Vec<ScheduleId>>,
    schedules: HashMap<ScheduleId, Schedule>,
}

impl UserDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-allocates the indexes for `users` users (avoids rehashing on bulk load).
    pub fn with_capacity(users: usize) -> Self {
        Self {
            users: HashMap::with_capacity(users),
            by_card: HashMap::with_capacity(users),
            ..Self::default()
        }
    }

    pub fn insert_schedule(&mut self, schedule: Schedule) {
        self.schedules.insert(schedule.id, schedule);
    }

    pub fn insert_group(&mut self, group: AccessGroup) -> Result<(), DirectoryError> {
        if let Some(p) = group
            .doors
            .iter()
            .find(|p| !self.schedules.contains_key(&p.schedule))
        {
            return Err(DirectoryError::UnknownSchedule(p.schedule));
        }
        self.door_index.retain(|(g, _), _| *g != group.id);
        for p in &group.doors {
            self.door_index
                .entry((group.id, p.door))
                .or_default()
                .push(p.schedule);
        }
        self.groups.insert(group.id, group);
        Ok(())
    }

    /// Inserts or replaces a user. Returns the previous record.
    pub fn upsert_user(&mut self, user: User) -> Result<Option<User>, DirectoryError> {
        if let Some(g) = user.groups.iter().find(|g| !self.groups.contains_key(g)) {
            return Err(DirectoryError::UnknownGroup(*g));
        }
        for card in &user.cards {
            if let Some(&owner) = self.by_card.get(card)
                && owner != user.id
            {
                return Err(DirectoryError::DuplicateCard {
                    card: card.clone(),
                    owner,
                });
            }
        }
        let previous = self.remove_user(user.id);
        for card in &user.cards {
            self.by_card.insert(card.clone(), user.id);
        }
        self.users.insert(user.id, user);
        Ok(previous)
    }

    pub fn remove_user(&mut self, id: UserId) -> Option<User> {
        let user = self.users.remove(&id)?;
        for card in &user.cards {
            self.by_card.remove(card);
        }
        Some(user)
    }

    pub fn user(&self, id: UserId) -> Option<&User> {
        self.users.get(&id)
    }

    pub fn user_by_card(&self, card: &CardId) -> Option<&User> {
        self.by_card.get(card).and_then(|id| self.users.get(id))
    }

    pub fn groups(&self) -> impl Iterator<Item = &AccessGroup> {
        self.groups.values()
    }

    pub fn schedules(&self) -> impl Iterator<Item = &Schedule> {
        self.schedules.values()
    }

    pub fn users(&self) -> impl Iterator<Item = &User> {
        self.users.values()
    }

    pub fn len(&self) -> usize {
        self.users.len()
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// Card, validity period, door permission and schedule check (no interlock rules).
    pub fn authorize_door(&self, card: &CardId, door: DoorId, at: NaiveDateTime) -> AccessDecision {
        let user = match self.valid_user(card, at) {
            Ok(user) => user,
            Err(denied) => return denied,
        };
        let mut permitted = false;
        for group in &user.groups {
            let Some(schedules) = self.door_index.get(&(*group, door)) else {
                continue;
            };
            permitted = true;
            if schedules
                .iter()
                .filter_map(|s| self.schedules.get(s))
                .any(|s| s.allows(at))
            {
                return AccessDecision::Granted { user: user.id };
            }
        }
        AccessDecision::Denied {
            user: Some(user.id),
            reason: if permitted {
                DenyReason::OutsideSchedule
            } else {
                DenyReason::NoPermission
            },
        }
    }

    /// Card, validity period and key-cabinet permission check (no interlock rules).
    pub fn authorize_key_cabinet(
        &self,
        card: &CardId,
        cabinet: KeyCabinetId,
        at: NaiveDateTime,
    ) -> AccessDecision {
        let user = match self.valid_user(card, at) {
            Ok(user) => user,
            Err(denied) => return denied,
        };
        let permitted = user
            .groups
            .iter()
            .filter_map(|g| self.groups.get(g))
            .any(|g| g.key_cabinets.contains(&cabinet));
        if permitted {
            AccessDecision::Granted { user: user.id }
        } else {
            AccessDecision::Denied {
                user: Some(user.id),
                reason: DenyReason::NoPermission,
            }
        }
    }

    fn valid_user(&self, card: &CardId, at: NaiveDateTime) -> Result<&User, AccessDecision> {
        let Some(user) = self.user_by_card(card) else {
            return Err(AccessDecision::Denied {
                user: None,
                reason: DenyReason::UnknownCard,
            });
        };
        let reason = match user.is_valid_at(at) {
            Validity::Valid => return Ok(user),
            Validity::NotYetValid => DenyReason::NotYetValid,
            Validity::Expired => DenyReason::Expired,
        };
        Err(AccessDecision::Denied {
            user: Some(user.id),
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DoorPermission, TimeRange};
    use chrono::{NaiveDate, NaiveTime};

    fn at(h: u32) -> NaiveDateTime {
        // Tuesday
        NaiveDate::from_ymd_opt(2026, 11, 17)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    fn card(s: &str) -> CardId {
        s.parse().unwrap()
    }

    fn directory() -> UserDirectory {
        let mut d = UserDirectory::new();
        d.insert_schedule(Schedule::always(ScheduleId(0)));
        let mut office = Schedule::always(ScheduleId(1));
        office.weekly = std::array::from_fn(|_| {
            vec![TimeRange {
                start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            }]
        });
        d.insert_schedule(office);
        d.insert_group(AccessGroup {
            id: GroupId(1),
            name: "staff".into(),
            doors: vec![
                DoorPermission {
                    door: DoorId(1),
                    schedule: ScheduleId(0),
                },
                DoorPermission {
                    door: DoorId(2),
                    schedule: ScheduleId(1),
                },
            ],
            key_cabinets: vec![KeyCabinetId(1)],
        })
        .unwrap();
        d.upsert_user(User {
            id: UserId(7),
            name: "alice".into(),
            cards: vec![card("01020304")],
            groups: vec![GroupId(1)],
            valid_from: at(0),
            valid_until: Some(at(20)),
        })
        .unwrap();
        d
    }

    fn denied(reason: DenyReason, user: Option<UserId>) -> AccessDecision {
        AccessDecision::Denied { user, reason }
    }

    #[test]
    fn grants_permitted_door_and_denies_others() {
        let d = directory();
        let alice = card("01020304");
        assert_eq!(
            d.authorize_door(&alice, DoorId(1), at(3)),
            AccessDecision::Granted { user: UserId(7) }
        );
        assert_eq!(
            d.authorize_door(&alice, DoorId(2), at(3)),
            denied(DenyReason::OutsideSchedule, Some(UserId(7)))
        );
        assert!(d.authorize_door(&alice, DoorId(2), at(10)).is_granted());
        assert_eq!(
            d.authorize_door(&alice, DoorId(3), at(10)),
            denied(DenyReason::NoPermission, Some(UserId(7)))
        );
        assert_eq!(
            d.authorize_door(&alice, DoorId(1), at(20)),
            denied(DenyReason::Expired, Some(UserId(7)))
        );
        assert_eq!(
            d.authorize_door(&card("FF"), DoorId(1), at(10)),
            denied(DenyReason::UnknownCard, None)
        );
        assert!(
            d.authorize_key_cabinet(&alice, KeyCabinetId(1), at(10))
                .is_granted()
        );
        assert!(
            !d.authorize_key_cabinet(&alice, KeyCabinetId(2), at(10))
                .is_granted()
        );
    }

    #[test]
    fn upsert_replaces_cards_and_rejects_duplicates() {
        let mut d = directory();
        let mut alice = d.user(UserId(7)).unwrap().clone();
        alice.cards = vec![card("AA")];
        d.upsert_user(alice).unwrap();
        assert!(d.user_by_card(&card("01020304")).is_none());
        assert_eq!(d.user_by_card(&card("AA")).unwrap().id, UserId(7));

        let bob = User {
            id: UserId(8),
            name: "bob".into(),
            cards: vec![card("AA")],
            groups: vec![],
            valid_from: at(0),
            valid_until: None,
        };
        assert_eq!(
            d.upsert_user(bob.clone()),
            Err(DirectoryError::DuplicateCard {
                card: card("AA"),
                owner: UserId(7)
            })
        );
        let ghost = User {
            groups: vec![GroupId(99)],
            cards: vec![],
            ..bob
        };
        assert_eq!(
            d.upsert_user(ghost),
            Err(DirectoryError::UnknownGroup(GroupId(99)))
        );
    }
}
