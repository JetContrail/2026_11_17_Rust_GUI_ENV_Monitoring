//! Device protocol layer for the Ethernet/IP monitoring network.
//!
//! * [`frame`]: card-reader frame format and stream decoder.
//! * [`message`]: device ↔ host messages carried inside encrypted records.
//! * [`crypto`]: AES-256-GCM records for devices that cannot speak TLS.
//! * [`tls`]: TLS 1.3 with mutual certificate authentication and a development PKI.
//! * [`transport`]: length-prefixed message links; every constructor requires TLS or AES-GCM.
//! * [`device`]: abstract traits for readers, locks, environment monitors, key cabinets and PDUs.
//! * [`host`]: applies device messages to the core [`envmon_core::system::System`].

pub mod crypto;
pub mod device;
pub mod frame;
pub mod host;
pub mod message;
pub mod tls;
pub mod transport;
