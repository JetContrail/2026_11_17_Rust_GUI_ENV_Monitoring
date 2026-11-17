//! Door supervision: lock state, door-open timer and the forced-open / open-too-long alarms.
//!
//! ```text
//! Closed --open while unlocked--> Open --open >= threshold--> OpenTooLong
//! Closed --open while locked----> ForcedOpen
//! OpenTooLong / ForcedOpen --acknowledge--> OpenAcknowledged
//! any open state --close--> Closed
//! ```
//!
//! While `OpenTooLong` or `ForcedOpen`, the alarm sound plays and the door icon blinks red.
//! Acknowledging stops both; closing the door restores the normal display.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::model::DoorConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockState {
    Locked,
    Unlocked,
    /// The lock reported a fault or does not respond.
    Fault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoorState {
    Closed,
    Open,
    OpenTooLong,
    ForcedOpen,
    /// Alarm acknowledged by an operator while the door is still open.
    OpenAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlarmKind {
    OpenTooLong,
    ForcedOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorEvent {
    AlarmRaised(AlarmKind),
    AlarmAcknowledged(AlarmKind),
    /// The door closed; the alarm (acknowledged or not) is over.
    AlarmCleared(AlarmKind),
    Relocked,
}

/// What the GUI shows for a door.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorIndication {
    pub door: DoorState,
    pub lock: LockState,
    /// Play the alarm sound.
    pub sound: bool,
    /// Blink the door icon red.
    pub blink: bool,
}

#[derive(Debug, Clone)]
pub struct DoorMonitor {
    unlock_duration: Duration,
    open_alarm_after: Duration,
    state: DoorState,
    lock: LockState,
    opened_at: Option<Instant>,
    unlocked_until: Option<Instant>,
    alarm: Option<AlarmKind>,
}

impl DoorMonitor {
    pub fn new(config: &DoorConfig) -> Self {
        Self {
            unlock_duration: config.unlock_duration,
            open_alarm_after: config.open_alarm_after,
            state: DoorState::Closed,
            lock: LockState::Locked,
            opened_at: None,
            unlocked_until: None,
            alarm: None,
        }
    }

    pub fn state(&self) -> DoorState {
        self.state
    }

    pub fn lock_state(&self) -> LockState {
        self.lock
    }

    pub fn unlock_duration(&self) -> Duration {
        self.unlock_duration
    }

    pub fn indication(&self) -> DoorIndication {
        let alarming = matches!(self.state, DoorState::OpenTooLong | DoorState::ForcedOpen);
        DoorIndication {
            door: self.state,
            lock: self.lock,
            sound: alarming,
            blink: alarming,
        }
    }

    /// Records that an unlock command was sent; the lock re-locks after `unlock_duration`.
    pub fn unlock(&mut self, now: Instant) {
        self.unlocked_until = Some(now + self.unlock_duration);
        if self.lock != LockState::Fault {
            self.lock = LockState::Unlocked;
        }
    }

    /// Lock state reported by the device (authoritative, e.g. a fault or manual key release).
    pub fn lock_reported(&mut self, state: LockState) {
        self.lock = state;
        if state == LockState::Locked {
            self.unlocked_until = None;
        }
    }

    pub fn door_sensor(&mut self, open: bool, now: Instant) -> Option<DoorEvent> {
        match (open, self.state) {
            (true, DoorState::Closed) => {
                self.opened_at = Some(now);
                if self.lock == LockState::Unlocked {
                    self.state = DoorState::Open;
                    None
                } else {
                    self.raise(AlarmKind::ForcedOpen)
                }
            }
            (true, _) | (false, DoorState::Closed) => None,
            (false, _) => {
                self.state = DoorState::Closed;
                self.opened_at = None;
                self.alarm.take().map(DoorEvent::AlarmCleared)
            }
        }
    }

    /// Advances timers. Call periodically (e.g. every 100 ms).
    pub fn tick(&mut self, now: Instant) -> Vec<DoorEvent> {
        let mut events = Vec::new();
        if self.unlocked_until.is_some_and(|until| now >= until) {
            self.unlocked_until = None;
            if self.lock == LockState::Unlocked {
                self.lock = LockState::Locked;
                events.push(DoorEvent::Relocked);
            }
        }
        if self.state == DoorState::Open
            && self
                .opened_at
                .is_some_and(|t| now.saturating_duration_since(t) >= self.open_alarm_after)
        {
            events.extend(self.raise(AlarmKind::OpenTooLong));
        }
        events
    }

    /// Operator acknowledgement: stops the sound and the blinking.
    pub fn acknowledge(&mut self) -> Option<DoorEvent> {
        if matches!(self.state, DoorState::OpenTooLong | DoorState::ForcedOpen) {
            self.state = DoorState::OpenAcknowledged;
            self.alarm.map(DoorEvent::AlarmAcknowledged)
        } else {
            None
        }
    }

    fn raise(&mut self, kind: AlarmKind) -> Option<DoorEvent> {
        self.state = match kind {
            AlarmKind::OpenTooLong => DoorState::OpenTooLong,
            AlarmKind::ForcedOpen => DoorState::ForcedOpen,
        };
        self.alarm = Some(kind);
        Some(DoorEvent::AlarmRaised(kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DoorId;

    const S: Duration = Duration::from_secs(1);

    fn monitor() -> (DoorMonitor, Instant) {
        (
            DoorMonitor::new(&DoorConfig::new(DoorId(1), "d")),
            Instant::now(),
        )
    }

    #[test]
    fn door_open_longer_than_threshold_raises_alarm() {
        let (mut d, t0) = monitor();
        d.unlock(t0);
        assert_eq!(d.lock_state(), LockState::Unlocked);
        assert_eq!(d.door_sensor(true, t0 + S), None);
        assert_eq!(d.state(), DoorState::Open);

        // Re-locks after the default 5 s, the door stays open.
        assert_eq!(d.tick(t0 + 5 * S), vec![DoorEvent::Relocked]);
        assert_eq!(d.tick(t0 + 10 * S), vec![]);
        assert!(!d.indication().sound);

        // Default threshold is 10 s from the moment the door opened.
        assert_eq!(
            d.tick(t0 + 11 * S),
            vec![DoorEvent::AlarmRaised(AlarmKind::OpenTooLong)]
        );
        let ind = d.indication();
        assert_eq!(ind.door, DoorState::OpenTooLong);
        assert!(ind.sound && ind.blink);
        // Raised once only.
        assert_eq!(d.tick(t0 + 20 * S), vec![]);

        assert_eq!(
            d.door_sensor(false, t0 + 21 * S),
            Some(DoorEvent::AlarmCleared(AlarmKind::OpenTooLong))
        );
        assert_eq!(d.indication().door, DoorState::Closed);
        assert!(!d.indication().blink);
    }

    #[test]
    fn closing_before_threshold_raises_nothing() {
        let (mut d, t0) = monitor();
        d.unlock(t0);
        d.door_sensor(true, t0);
        assert_eq!(d.tick(t0 + 9 * S), vec![DoorEvent::Relocked]);
        assert_eq!(d.door_sensor(false, t0 + 9 * S), None);
        assert_eq!(d.tick(t0 + 30 * S), vec![]);
        assert_eq!(d.state(), DoorState::Closed);
    }

    #[test]
    fn acknowledge_stops_sound_and_blink_until_closed() {
        let (mut d, t0) = monitor();
        d.unlock(t0);
        d.door_sensor(true, t0);
        d.tick(t0 + 10 * S);
        assert_eq!(
            d.acknowledge(),
            Some(DoorEvent::AlarmAcknowledged(AlarmKind::OpenTooLong))
        );
        let ind = d.indication();
        assert_eq!(ind.door, DoorState::OpenAcknowledged);
        assert!(!ind.sound && !ind.blink);
        assert_eq!(d.acknowledge(), None);
        assert_eq!(d.tick(t0 + 60 * S), vec![]);
        assert_eq!(
            d.door_sensor(false, t0 + 61 * S),
            Some(DoorEvent::AlarmCleared(AlarmKind::OpenTooLong))
        );
    }

    #[test]
    fn opening_while_locked_is_forced_open() {
        let (mut d, t0) = monitor();
        assert_eq!(
            d.door_sensor(true, t0),
            Some(DoorEvent::AlarmRaised(AlarmKind::ForcedOpen))
        );
        assert!(d.indication().blink);
        assert_eq!(d.tick(t0 + 30 * S), vec![]);
        assert_eq!(
            d.door_sensor(false, t0 + 31 * S),
            Some(DoorEvent::AlarmCleared(AlarmKind::ForcedOpen))
        );
    }

    #[test]
    fn threshold_is_configurable_per_door() {
        let mut config = DoorConfig::new(DoorId(1), "d");
        config.open_alarm_after = 3 * S;
        let mut d = DoorMonitor::new(&config);
        let t0 = Instant::now();
        d.unlock(t0);
        d.door_sensor(true, t0);
        assert_eq!(
            d.tick(t0 + 3 * S),
            vec![DoorEvent::AlarmRaised(AlarmKind::OpenTooLong)]
        );
    }

    #[test]
    fn lock_fault_is_reported_and_kept() {
        let (mut d, t0) = monitor();
        d.lock_reported(LockState::Fault);
        d.unlock(t0);
        assert_eq!(d.lock_state(), LockState::Fault);
        assert_eq!(d.tick(t0 + 10 * S), vec![]);
        assert_eq!(d.indication().lock, LockState::Fault);
    }
}
