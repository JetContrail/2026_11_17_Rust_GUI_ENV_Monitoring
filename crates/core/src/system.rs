//! Control system facade: ties the directory, interlock rules, door supervision, alarms
//! and object state together and queues commands for the device layer.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::directory::{AccessDecision, DenyReason, DirectoryError, UserDirectory};
use crate::door::{AlarmKind, DoorEvent, DoorIndication, DoorMonitor, LockState};
use crate::model::{CardId, DoorConfig, DoorId, KeyCabinetId, ReaderId, User, UserId};
use crate::objects::{ObjectError, ObjectKind, ObjectRegistry, ObjectValue};
use crate::rules::{PresenceLog, RuleEngine, Target};
use crate::store::{Store, StoreError};

/// Number of access records and alarms kept in memory (older ones stay in the store).
pub const HISTORY_CAPACITY: usize = 10_000;

/// Command for the device layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ControlCommand {
    Unlock {
        door: DoorId,
        duration_ms: u32,
    },
    /// LED / buzzer feedback on the reader that read the card.
    ReaderFeedback {
        reader: ReaderId,
        granted: bool,
    },
    ReleaseKey {
        cabinet: KeyCabinetId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessPoint {
    Door(DoorId),
    /// Unlock requested by an operator or the WebAPI.
    RemoteUnlock(DoorId),
    KeyCabinet(KeyCabinetId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRecord {
    pub at: NaiveDateTime,
    pub point: AccessPoint,
    pub card: Option<CardId>,
    pub user: Option<UserId>,
    pub granted: bool,
    pub reason: Option<DenyReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub door: DoorId,
    pub kind: AlarmKind,
    pub raised_at: NaiveDateTime,
    pub acknowledged_at: Option<NaiveDateTime>,
    pub cleared_at: Option<NaiveDateTime>,
}

impl Alarm {
    pub fn is_active(&self) -> bool {
        self.cleared_at.is_none()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("unknown door {0}")]
    UnknownDoor(DoorId),
    #[error("unknown reader {0}")]
    UnknownReader(ReaderId),
    #[error("reader {0} is assigned to more than one door")]
    DuplicateReader(ReaderId),
    #[error(transparent)]
    Directory(#[from] DirectoryError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug)]
struct Door {
    config: DoorConfig,
    monitor: DoorMonitor,
}

#[derive(Debug)]
pub struct System {
    directory: UserDirectory,
    rules: RuleEngine,
    presence: PresenceLog,
    doors: HashMap<DoorId, Door>,
    reader_door: HashMap<ReaderId, DoorId>,
    objects: ObjectRegistry,
    alarms: VecDeque<Alarm>,
    history: VecDeque<AccessRecord>,
    commands: Vec<ControlCommand>,
    store: Option<Store>,
}

impl System {
    pub fn new(
        directory: UserDirectory,
        rules: RuleEngine,
        doors: impl IntoIterator<Item = DoorConfig>,
    ) -> Result<Self, SystemError> {
        let mut system = Self {
            directory,
            rules,
            presence: PresenceLog::default(),
            doors: HashMap::new(),
            reader_door: HashMap::new(),
            objects: ObjectRegistry::new(),
            alarms: VecDeque::new(),
            history: VecDeque::new(),
            commands: Vec::new(),
            store: None,
        };
        for config in doors {
            for &reader in &config.readers {
                if system.reader_door.insert(reader, config.id).is_some() {
                    return Err(SystemError::DuplicateReader(reader));
                }
            }
            let monitor = DoorMonitor::new(&config);
            if let Some(object) = config.object {
                system
                    .objects
                    .register(object, ObjectKind::Door, config.name.clone())?;
                system
                    .objects
                    .update(object, ObjectValue::Door(monitor.indication()))?;
            }
            system.doors.insert(config.id, Door { config, monitor });
        }
        Ok(system)
    }

    /// Persists access history and user changes to `store`, and resumes the in-memory
    /// history from the records already stored.
    pub fn with_store(mut self, store: Store) -> Result<Self, SystemError> {
        self.history = store
            .recent_access(HISTORY_CAPACITY)?
            .into_iter()
            .rev()
            .collect();
        self.store = Some(store);
        Ok(self)
    }

    pub fn directory(&self) -> &UserDirectory {
        &self.directory
    }

    pub fn rules(&self) -> &RuleEngine {
        &self.rules
    }

    pub fn objects(&self) -> &ObjectRegistry {
        &self.objects
    }

    /// For sensor values etc. reported by the device layer.
    pub fn objects_mut(&mut self) -> &mut ObjectRegistry {
        &mut self.objects
    }

    pub fn upsert_user(&mut self, user: User) -> Result<(), SystemError> {
        if let Some(store) = &mut self.store {
            // Validate before persisting so the store never holds what the directory rejects.
            let mut check = self.directory.clone();
            check.upsert_user(user.clone())?;
            store.save_users(std::slice::from_ref(&user))?;
        }
        self.directory.upsert_user(user)?;
        Ok(())
    }

    pub fn door_indication(&self, door: DoorId) -> Option<DoorIndication> {
        self.doors.get(&door).map(|d| d.monitor.indication())
    }

    pub fn door_configs(&self) -> impl Iterator<Item = &DoorConfig> {
        self.doors.values().map(|d| &d.config)
    }

    /// Alarms, newest last.
    pub fn alarms(&self) -> impl Iterator<Item = &Alarm> {
        self.alarms.iter()
    }

    pub fn active_alarms(&self) -> impl Iterator<Item = &Alarm> {
        self.alarms.iter().filter(|a| a.is_active())
    }

    /// Access history kept in memory, newest last.
    pub fn history(&self) -> impl DoubleEndedIterator<Item = &AccessRecord> {
        self.history.iter()
    }

    /// Commands queued for the device layer since the last call.
    pub fn take_commands(&mut self) -> Vec<ControlCommand> {
        std::mem::take(&mut self.commands)
    }

    /// A card was read at `reader`: authorize, unlock and record.
    pub fn card_presented(
        &mut self,
        reader: ReaderId,
        card: CardId,
        at: NaiveDateTime,
        now: Instant,
    ) -> Result<AccessDecision, SystemError> {
        let door_id = *self
            .reader_door
            .get(&reader)
            .ok_or(SystemError::UnknownReader(reader))?;
        let mut decision = self.directory.authorize_door(&card, door_id, at);
        if let AccessDecision::Granted { user } = decision {
            decision = self.apply_rules(user, Target::Door(door_id), at);
        }
        let granted = decision.is_granted();
        if let AccessDecision::Granted { user } = decision {
            let door = self
                .doors
                .get(&door_id)
                .ok_or(SystemError::UnknownDoor(door_id))?;
            if let Some(area) = door.config.enters_area {
                self.presence.record_entry(user, area, at);
            }
            self.unlock(door_id, now)?;
        }
        self.commands
            .push(ControlCommand::ReaderFeedback { reader, granted });
        self.record(AccessPoint::Door(door_id), Some(card), &decision, at)?;
        Ok(decision)
    }

    /// A card was read at a key cabinet: authorize, release the key and record.
    pub fn key_requested(
        &mut self,
        cabinet: KeyCabinetId,
        card: CardId,
        at: NaiveDateTime,
    ) -> Result<AccessDecision, SystemError> {
        let mut decision = self.directory.authorize_key_cabinet(&card, cabinet, at);
        if let AccessDecision::Granted { user } = decision {
            decision = self.apply_rules(user, Target::KeyCabinet(cabinet), at);
        }
        if decision.is_granted() {
            self.commands.push(ControlCommand::ReleaseKey { cabinet });
        }
        self.record(AccessPoint::KeyCabinet(cabinet), Some(card), &decision, at)?;
        Ok(decision)
    }

    /// Unlock requested by an operator or the WebAPI (bypasses card checks).
    pub fn remote_unlock(
        &mut self,
        door: DoorId,
        at: NaiveDateTime,
        now: Instant,
    ) -> Result<(), SystemError> {
        self.unlock(door, now)?;
        let rec = AccessRecord {
            at,
            point: AccessPoint::RemoteUnlock(door),
            card: None,
            user: None,
            granted: true,
            reason: None,
        };
        self.push_record(rec)
    }

    pub fn door_sensor(
        &mut self,
        door: DoorId,
        open: bool,
        at: NaiveDateTime,
        now: Instant,
    ) -> Result<(), SystemError> {
        let d = self.door_mut(door)?;
        let event = d.monitor.door_sensor(open, now);
        self.apply_events(door, event, at)
    }

    pub fn lock_reported(&mut self, door: DoorId, state: LockState) -> Result<(), SystemError> {
        self.door_mut(door)?.monitor.lock_reported(state);
        self.apply_events(door, None, NaiveDateTime::MIN)
    }

    /// Operator acknowledgement of the door's alarm. Returns `false` if nothing was alarming.
    pub fn acknowledge(&mut self, door: DoorId, at: NaiveDateTime) -> Result<bool, SystemError> {
        let event = self.door_mut(door)?.monitor.acknowledge();
        let acknowledged = event.is_some();
        self.apply_events(door, event, at)?;
        Ok(acknowledged)
    }

    /// Advances all door timers (re-lock, open-too-long alarm).
    pub fn tick(&mut self, at: NaiveDateTime, now: Instant) -> Result<(), SystemError> {
        let mut fired = Vec::new();
        for (&id, door) in &mut self.doors {
            let events = door.monitor.tick(now);
            if !events.is_empty() {
                fired.push((id, events));
            }
        }
        for (id, events) in fired {
            self.apply_events(id, events, at)?;
        }
        Ok(())
    }

    fn apply_rules(&self, user: UserId, target: Target, at: NaiveDateTime) -> AccessDecision {
        match self.rules.check(user, target, at, &self.presence) {
            Ok(()) => AccessDecision::Granted { user },
            Err(v) => AccessDecision::Denied {
                user: Some(user),
                reason: DenyReason::Interlock {
                    rule: v.rule,
                    name: v.name,
                },
            },
        }
    }

    fn unlock(&mut self, door_id: DoorId, now: Instant) -> Result<(), SystemError> {
        let door = self.door_mut(door_id)?;
        door.monitor.unlock(now);
        let duration = door.monitor.unlock_duration();
        self.commands.push(ControlCommand::Unlock {
            door: door_id,
            duration_ms: duration_ms(duration),
        });
        self.apply_events(door_id, None, NaiveDateTime::MIN)
    }

    fn door_mut(&mut self, door: DoorId) -> Result<&mut Door, SystemError> {
        self.doors
            .get_mut(&door)
            .ok_or(SystemError::UnknownDoor(door))
    }

    fn apply_events(
        &mut self,
        door: DoorId,
        events: impl IntoIterator<Item = DoorEvent>,
        at: NaiveDateTime,
    ) -> Result<(), SystemError> {
        for event in events {
            match event {
                DoorEvent::AlarmRaised(kind) => {
                    if self.alarms.len() >= HISTORY_CAPACITY
                        && let Some(i) = self.alarms.iter().position(|a| !a.is_active())
                    {
                        self.alarms.remove(i);
                    }
                    self.alarms.push_back(Alarm {
                        door,
                        kind,
                        raised_at: at,
                        acknowledged_at: None,
                        cleared_at: None,
                    });
                }
                DoorEvent::AlarmAcknowledged(_) => {
                    if let Some(a) = self.active_alarm_mut(door) {
                        a.acknowledged_at = Some(at);
                    }
                }
                DoorEvent::AlarmCleared(_) => {
                    if let Some(a) = self.active_alarm_mut(door) {
                        a.cleared_at = Some(at);
                    }
                }
                DoorEvent::Relocked => {}
            }
        }
        let d = self
            .doors
            .get(&door)
            .ok_or(SystemError::UnknownDoor(door))?;
        if let Some(object) = d.config.object {
            self.objects
                .update(object, ObjectValue::Door(d.monitor.indication()))?;
        }
        Ok(())
    }

    fn active_alarm_mut(&mut self, door: DoorId) -> Option<&mut Alarm> {
        self.alarms
            .iter_mut()
            .rev()
            .find(|a| a.door == door && a.is_active())
    }

    fn record(
        &mut self,
        point: AccessPoint,
        card: Option<CardId>,
        decision: &AccessDecision,
        at: NaiveDateTime,
    ) -> Result<(), SystemError> {
        let rec = AccessRecord {
            at,
            point,
            card,
            user: decision.user(),
            granted: decision.is_granted(),
            reason: match decision {
                AccessDecision::Granted { .. } => None,
                AccessDecision::Denied { reason, .. } => Some(reason.clone()),
            },
        };
        self.push_record(rec)
    }

    fn push_record(&mut self, rec: AccessRecord) -> Result<(), SystemError> {
        let stored = self.store.as_ref().map(|s| s.append_access(&rec));
        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(rec);
        stored.transpose()?;
        Ok(())
    }
}

fn duration_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}
