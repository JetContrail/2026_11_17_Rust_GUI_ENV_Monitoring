//! Messages exchanged between devices and the host, carried as JSON inside TLS or
//! AES-256-GCM records.

use envmon_core::door::LockState;
use envmon_core::model::{CardId, DoorId, KeyCabinetId, ObjectId};
use envmon_core::system::ControlCommand;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantity {
    TemperatureC,
    HumidityPct,
    Volts,
    Amperes,
}

/// Device → host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Uplink {
    Hello {
        device: String,
    },
    Heartbeat,
    /// Raw card-reader frame ([`crate::frame::ReaderFrame`]).
    ReaderFrame {
        frame: Vec<u8>,
    },
    DoorSensor {
        door: DoorId,
        open: bool,
    },
    LockStatus {
        door: DoorId,
        state: LockState,
    },
    Analog {
        object: ObjectId,
        quantity: Quantity,
        value: f32,
    },
    DigitalInput {
        object: ObjectId,
        on: bool,
    },
    /// A card was presented at a key cabinet.
    KeyRequest {
        cabinet: KeyCabinetId,
        card: CardId,
    },
    KeyCabinet {
        object: ObjectId,
        keys_out: u16,
    },
    Outlet {
        object: ObjectId,
        on: bool,
        watts: f32,
    },
}

/// Host → device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Downlink {
    Control(ControlCommand),
    SetOutput { object: ObjectId, on: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_serialise_with_a_type_tag() {
        let up = Uplink::DoorSensor {
            door: DoorId(3),
            open: true,
        };
        let json = serde_json::to_string(&up).unwrap();
        assert_eq!(json, r#"{"type":"door_sensor","door":3,"open":true}"#);
        assert_eq!(serde_json::from_str::<Uplink>(&json).unwrap(), up);

        let down = Downlink::Control(ControlCommand::Unlock {
            door: DoorId(3),
            duration_ms: 5000,
        });
        let json = serde_json::to_string(&down).unwrap();
        assert_eq!(
            json,
            r#"{"type":"control","command":"unlock","door":3,"duration_ms":5000}"#
        );
        assert_eq!(serde_json::from_str::<Downlink>(&json).unwrap(), down);
    }
}
