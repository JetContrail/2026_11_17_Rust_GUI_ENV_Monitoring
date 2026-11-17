//! Interlock rule engine: conditions that must hold before a door or key cabinet opens,
//! e.g. "the rack door opens only for users who entered the server room in the last hour".

use std::collections::HashMap;

use chrono::{NaiveDateTime, TimeDelta};
use serde::{Deserialize, Serialize};

use crate::model::{AreaId, DoorId, KeyCabinetId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleId(pub u32);

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What a rule protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Door(DoorId),
    KeyCabinet(KeyCabinetId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// The user entered `area` (by a granted door access), optionally within the last `within_secs`.
    EnteredArea {
        area: AreaId,
        within_secs: Option<u64>,
    },
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

impl Condition {
    fn holds(&self, user: UserId, at: NaiveDateTime, presence: &PresenceLog) -> bool {
        match self {
            Self::EnteredArea { area, within_secs } => {
                presence.last_entry(user, *area).is_some_and(|entered| {
                    entered <= at
                        && within_secs.is_none_or(|secs| {
                            at - entered
                                <= TimeDelta::seconds(i64::try_from(secs).unwrap_or(i64::MAX))
                        })
                })
            }
            Self::All(cs) => cs.iter().all(|c| c.holds(user, at, presence)),
            Self::Any(cs) => cs.iter().any(|c| c.holds(user, at, presence)),
            Self::Not(c) => !c.holds(user, at, presence),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub name: String,
    pub target: Target,
    pub condition: Condition,
}

/// Last time each user entered each area.
#[derive(Debug, Default, Clone)]
pub struct PresenceLog {
    last_entry: HashMap<(UserId, AreaId), NaiveDateTime>,
}

impl PresenceLog {
    pub fn record_entry(&mut self, user: UserId, area: AreaId, at: NaiveDateTime) {
        self.last_entry.insert((user, area), at);
    }

    pub fn last_entry(&self, user: UserId, area: AreaId) -> Option<NaiveDateTime> {
        self.last_entry.get(&(user, area)).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("interlock rule {rule} ({name}) is not satisfied")]
pub struct RuleViolation {
    pub rule: RuleId,
    pub name: String,
}

#[derive(Debug, Default, Clone)]
pub struct RuleEngine {
    rules: HashMap<Target, Vec<Rule>>,
}

impl RuleEngine {
    pub fn new(rules: impl IntoIterator<Item = Rule>) -> Self {
        let mut engine = Self::default();
        for rule in rules {
            engine.add(rule);
        }
        engine
    }

    pub fn add(&mut self, rule: Rule) {
        self.rules.entry(rule.target).or_default().push(rule);
    }

    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.values().flatten()
    }

    /// All rules attached to `target` must hold; the first failing rule is reported.
    pub fn check(
        &self,
        user: UserId,
        target: Target,
        at: NaiveDateTime,
        presence: &PresenceLog,
    ) -> Result<(), RuleViolation> {
        self.rules
            .get(&target)
            .into_iter()
            .flatten()
            .find(|r| !r.condition.holds(user, at, presence))
            .map_or(Ok(()), |r| {
                Err(RuleViolation {
                    rule: r.id,
                    name: r.name.clone(),
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 17)
            .unwrap()
            .and_hms_opt(10, m, 0)
            .unwrap()
    }

    const ROOM: AreaId = AreaId(1);
    const LAB: AreaId = AreaId(2);
    const U: UserId = UserId(1);

    fn engine(condition: Condition) -> RuleEngine {
        RuleEngine::new([Rule {
            id: RuleId(1),
            name: "rack".into(),
            target: Target::Door(DoorId(9)),
            condition,
        }])
    }

    #[test]
    fn entered_area_respects_time_window() {
        let e = engine(Condition::EnteredArea {
            area: ROOM,
            within_secs: Some(600),
        });
        let mut p = PresenceLog::default();
        let rack = Target::Door(DoorId(9));
        assert!(e.check(U, rack, at(0), &p).is_err());
        p.record_entry(U, ROOM, at(0));
        assert!(e.check(U, rack, at(10), &p).is_ok());
        assert!(e.check(U, rack, at(11), &p).is_err());
        // Unrelated targets have no rules.
        assert!(
            e.check(U, Target::Door(DoorId(1)), at(0), &PresenceLog::default())
                .is_ok()
        );
    }

    #[test]
    fn combinators() {
        let entered = |area| Condition::EnteredArea {
            area,
            within_secs: None,
        };
        let mut p = PresenceLog::default();
        p.record_entry(U, ROOM, at(0));
        let rack = Target::Door(DoorId(9));
        let check = |c: Condition, p: &PresenceLog| engine(c).check(U, rack, at(1), p).is_ok();
        assert!(check(Condition::Any(vec![entered(ROOM), entered(LAB)]), &p));
        assert!(!check(
            Condition::All(vec![entered(ROOM), entered(LAB)]),
            &p
        ));
        assert!(check(Condition::Not(Box::new(entered(LAB))), &p));
        p.record_entry(U, LAB, at(0));
        assert!(check(Condition::All(vec![entered(ROOM), entered(LAB)]), &p));
    }

    #[test]
    fn rules_deserialize_from_json() {
        let rule: Rule = serde_json::from_str(
            r#"{"id":1,"name":"key","target":{"key_cabinet":3},
                "condition":{"entered_area":{"area":1,"within_secs":null}}}"#,
        )
        .unwrap();
        assert_eq!(rule.target, Target::KeyCabinet(KeyCabinetId(3)));
    }
}
