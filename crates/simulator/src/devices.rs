//! Virtual devices implementing the [`envmon_protocol::device`] traits.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use envmon_core::door::LockState;
use envmon_core::model::{CardId, DoorId, KeyCabinetId, ObjectId, ReaderId};
use envmon_protocol::device::{
    CardReader, DeviceError, ElectricLock, EnvironmentMonitor, KeyCabinet, OutletReading, Pdu,
};
use envmon_protocol::message::Quantity;

#[derive(Debug)]
pub struct VirtualReader {
    id: ReaderId,
    pending: VecDeque<CardId>,
    /// Last LED / buzzer result shown.
    pub last_feedback: Option<bool>,
}

impl VirtualReader {
    pub fn new(id: ReaderId) -> Self {
        Self {
            id,
            pending: VecDeque::new(),
            last_feedback: None,
        }
    }

    pub fn touch(&mut self, card: CardId) {
        self.pending.push_back(card);
    }
}

impl CardReader for VirtualReader {
    fn reader_id(&self) -> ReaderId {
        self.id
    }

    fn poll_card(&mut self) -> Option<CardId> {
        self.pending.pop_front()
    }

    fn feedback(&mut self, granted: bool) -> Result<(), DeviceError> {
        self.last_feedback = Some(granted);
        Ok(())
    }
}

#[derive(Debug)]
pub struct VirtualLock {
    door: DoorId,
    open: bool,
    unlocked_until: Option<Instant>,
    now: Instant,
}

impl VirtualLock {
    pub fn new(door: DoorId, now: Instant) -> Self {
        Self {
            door,
            open: false,
            unlocked_until: None,
            now,
        }
    }

    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Advances the lock's clock (it re-locks by itself when the unlock time ends).
    pub fn set_time(&mut self, now: Instant) {
        self.now = now;
    }
}

impl ElectricLock for VirtualLock {
    fn door_id(&self) -> DoorId {
        self.door
    }

    fn unlock(&mut self, duration: Duration) -> Result<(), DeviceError> {
        self.unlocked_until = Some(self.now + duration);
        Ok(())
    }

    fn lock_state(&self) -> LockState {
        match self.unlocked_until {
            Some(until) if self.now < until => LockState::Unlocked,
            _ => LockState::Locked,
        }
    }

    fn door_open(&self) -> bool {
        self.open
    }
}

#[derive(Debug, Default)]
pub struct VirtualEnvironment {
    analog: BTreeMap<ObjectId, (Quantity, f32)>,
    inputs: BTreeMap<ObjectId, bool>,
    pub outputs: BTreeMap<ObjectId, bool>,
}

impl VirtualEnvironment {
    pub fn add_analog(&mut self, object: ObjectId, quantity: Quantity, value: f32) {
        self.analog.insert(object, (quantity, value));
    }

    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.analog
            .keys()
            .chain(self.inputs.keys())
            .chain(self.outputs.keys())
            .copied()
    }

    pub fn set_analog(&mut self, object: ObjectId, value: f32) -> Result<Quantity, DeviceError> {
        let entry = self
            .analog
            .get_mut(&object)
            .ok_or(DeviceError::UnknownChannel(channel(object)))?;
        entry.1 = value;
        Ok(entry.0)
    }

    pub fn set_input(&mut self, object: ObjectId, on: bool) {
        self.inputs.insert(object, on);
    }
}

fn channel(object: ObjectId) -> u16 {
    u16::try_from(object.0).unwrap_or(u16::MAX)
}

impl EnvironmentMonitor for VirtualEnvironment {
    fn read_analog(&mut self) -> Result<Vec<(ObjectId, Quantity, f32)>, DeviceError> {
        Ok(self.analog.iter().map(|(&o, &(q, v))| (o, q, v)).collect())
    }

    fn read_inputs(&mut self) -> Result<Vec<(ObjectId, bool)>, DeviceError> {
        Ok(self.inputs.iter().map(|(&o, &on)| (o, on)).collect())
    }

    fn set_output(&mut self, object: ObjectId, on: bool) -> Result<(), DeviceError> {
        self.outputs.insert(object, on);
        Ok(())
    }
}

#[derive(Debug)]
pub struct VirtualKeyCabinet {
    id: KeyCabinetId,
    keys_out: u16,
    capacity: u16,
}

impl VirtualKeyCabinet {
    pub fn new(id: KeyCabinetId, capacity: u16) -> Self {
        Self {
            id,
            keys_out: 0,
            capacity,
        }
    }
}

impl KeyCabinet for VirtualKeyCabinet {
    fn cabinet_id(&self) -> KeyCabinetId {
        self.id
    }

    fn release_key(&mut self) -> Result<(), DeviceError> {
        if self.keys_out >= self.capacity {
            return Err(DeviceError::Denied("no key left".into()));
        }
        self.keys_out += 1;
        Ok(())
    }

    fn keys_out(&self) -> u16 {
        self.keys_out
    }
}

#[derive(Debug, Default)]
pub struct VirtualPdu {
    outlets: BTreeMap<ObjectId, (bool, f32)>,
}

impl VirtualPdu {
    pub fn add_outlet(&mut self, object: ObjectId, watts: f32) {
        self.outlets.insert(object, (true, watts));
    }

    pub fn objects(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.outlets.keys().copied()
    }

    pub fn set_load(&mut self, object: ObjectId, on: bool, watts: f32) {
        self.outlets.insert(object, (on, watts));
    }

    pub fn has(&self, object: ObjectId) -> bool {
        self.outlets.contains_key(&object)
    }
}

impl Pdu for VirtualPdu {
    fn outlets(&mut self) -> Result<Vec<OutletReading>, DeviceError> {
        Ok(self
            .outlets
            .iter()
            .map(|(&object, &(on, watts))| OutletReading {
                object,
                on,
                watts: if on { watts } else { 0.0 },
            })
            .collect())
    }

    fn set_outlet(&mut self, object: ObjectId, on: bool) -> Result<(), DeviceError> {
        let outlet = self
            .outlets
            .get_mut(&object)
            .ok_or(DeviceError::UnknownChannel(channel(object)))?;
        outlet.0 = on;
        Ok(())
    }
}
