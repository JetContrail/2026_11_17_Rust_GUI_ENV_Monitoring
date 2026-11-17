//! Abstract device interfaces. Real-device adapters and the simulator implement these, so the
//! host logic can be tested without hardware.

use std::time::Duration;

use envmon_core::door::LockState;
use envmon_core::model::{CardId, DoorId, KeyCabinetId, ObjectId, ReaderId};

use crate::message::Quantity;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("device is offline")]
    Offline,
    #[error("device reported a fault: {0}")]
    Fault(String),
    #[error("unknown channel {0}")]
    UnknownChannel(u16),
    #[error("operation not permitted: {0}")]
    Denied(String),
}

/// Card reader (SS2269VHC-Lite / SS2279VHC-RS-FS class).
pub trait CardReader {
    fn reader_id(&self) -> ReaderId;
    /// Next card presented since the last call.
    fn poll_card(&mut self) -> Option<CardId>;
    /// LED / buzzer result of the access decision.
    fn feedback(&mut self, granted: bool) -> Result<(), DeviceError>;
}

/// Electric lock with door sensor.
pub trait ElectricLock {
    fn door_id(&self) -> DoorId;
    fn unlock(&mut self, duration: Duration) -> Result<(), DeviceError>;
    fn lock_state(&self) -> LockState;
    fn door_open(&self) -> bool;
}

/// Environment monitoring unit (DT-M01 / ND-EW04E / ND-IOTR / ND-SW01 class).
pub trait EnvironmentMonitor {
    /// Analog channels: object, quantity and current value.
    fn read_analog(&mut self) -> Result<Vec<(ObjectId, Quantity, f32)>, DeviceError>;
    fn read_inputs(&mut self) -> Result<Vec<(ObjectId, bool)>, DeviceError>;
    fn set_output(&mut self, object: ObjectId, on: bool) -> Result<(), DeviceError>;
}

/// Key cabinet (SS2280BL-E class).
pub trait KeyCabinet {
    fn cabinet_id(&self) -> KeyCabinetId;
    /// Unlocks a key slot after the host has granted the request.
    fn release_key(&mut self) -> Result<(), DeviceError>;
    fn keys_out(&self) -> u16;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutletReading {
    pub object: ObjectId,
    pub on: bool,
    pub watts: f32,
}

/// Intelligent PDU (Geist class).
pub trait Pdu {
    fn outlets(&mut self) -> Result<Vec<OutletReading>, DeviceError>;
    fn set_outlet(&mut self, object: ObjectId, on: bool) -> Result<(), DeviceError>;
}
