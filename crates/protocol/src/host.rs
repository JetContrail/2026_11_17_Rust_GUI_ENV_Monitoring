//! Host side: applies device messages to the core [`System`] and returns the resulting
//! commands as downlink messages.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use chrono::NaiveDateTime;
use envmon_core::objects::{ObjectError, ObjectValue};
use envmon_core::system::{System, SystemError};
use envmon_core::{Level, Thresholds};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::frame::{FrameError, ReaderFrame};
use crate::message::{Downlink, Quantity, Uplink};
use crate::transport::{Link, LinkError};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    System(#[from] SystemError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error("unexpected frame from a device: {0:?}")]
    UnexpectedFrame(ReaderFrame),
}

/// What the host learned from a message besides the commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    None,
    Tamper(envmon_core::model::ReaderId),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HostConfig {
    pub thresholds: Thresholds,
}

impl HostConfig {
    fn level(&self, quantity: Quantity, value: f32) -> Level {
        match quantity {
            Quantity::TemperatureC => self.thresholds.temperature_c.classify(value),
            Quantity::HumidityPct => self.thresholds.humidity_pct.classify(value),
            Quantity::Volts | Quantity::Amperes => Level::Normal,
        }
    }
}

/// Source of wall-clock time (history, alarms) and monotonic time (door timers).
pub trait Clock: Send + Sync {
    fn now(&self) -> (NaiveDateTime, Instant);
}

/// The local system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> (NaiveDateTime, Instant) {
        (chrono::Local::now().naive_local(), Instant::now())
    }
}

/// Serves one device link: for every uplink the door timers are advanced, the message is
/// applied, and the resulting downlinks are sent back as one reply (`Vec<Downlink>`).
/// Returns `Ok(())` when the device closes the connection.
pub async fn serve_link<S: AsyncRead + AsyncWrite + Unpin>(
    mut link: Link<S>,
    system: Arc<Mutex<System>>,
    config: HostConfig,
    clock: Arc<dyn Clock>,
) -> Result<(), ServeError> {
    loop {
        let message: Uplink = match link.recv().await {
            Ok(m) => m,
            Err(LinkError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        let reply = {
            let (at, now) = clock.now();
            let mut sys = system.lock().unwrap_or_else(PoisonError::into_inner);
            sys.tick(at, now)?;
            apply(&mut sys, &config, &message, at, now)?.1
        };
        link.send(&reply).await?;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    System(#[from] SystemError),
}

/// Applies `message` and drains the commands it caused.
pub fn apply(
    system: &mut System,
    config: &HostConfig,
    message: &Uplink,
    at: NaiveDateTime,
    now: Instant,
) -> Result<(Notice, Vec<Downlink>), HostError> {
    let mut notice = Notice::None;
    match message {
        Uplink::Hello { .. } | Uplink::Heartbeat => {}
        Uplink::ReaderFrame { frame } => match ReaderFrame::parse(frame)? {
            ReaderFrame::CardRead { reader, card } => {
                system.card_presented(reader, card, at, now)?;
            }
            ReaderFrame::Tamper { reader } => notice = Notice::Tamper(reader),
            ReaderFrame::Heartbeat { .. } => {}
            f @ ReaderFrame::Feedback { .. } => return Err(HostError::UnexpectedFrame(f)),
        },
        Uplink::DoorSensor { door, open } => system.door_sensor(*door, *open, at, now)?,
        Uplink::LockStatus { door, state } => system.lock_reported(*door, *state)?,
        Uplink::Analog {
            object,
            quantity,
            value,
        } => {
            let level = config.level(*quantity, *value);
            system.objects_mut().update(
                *object,
                ObjectValue::Analog {
                    value: *value,
                    level,
                },
            )?;
        }
        Uplink::DigitalInput { object, on } => {
            system
                .objects_mut()
                .update(*object, ObjectValue::Digital { on: *on })?;
        }
        Uplink::KeyRequest { cabinet, card } => {
            system.key_requested(*cabinet, card.clone(), at)?;
        }
        Uplink::KeyCabinet { object, keys_out } => {
            system.objects_mut().update(
                *object,
                ObjectValue::KeyCabinet {
                    keys_out: *keys_out,
                },
            )?;
        }
        Uplink::Outlet { object, on, watts } => {
            system.objects_mut().update(
                *object,
                ObjectValue::Outlet {
                    on: *on,
                    watts: *watts,
                },
            )?;
        }
    }
    let commands = system
        .take_commands()
        .into_iter()
        .map(Downlink::Control)
        .collect();
    Ok((notice, commands))
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use envmon_core::demo::{self, KEY_CABINET, RACK_READER, ROOM_DOOR, ROOM_READER};
    use envmon_core::model::ObjectId;
    use envmon_core::objects::ObjectKind;
    use envmon_core::system::ControlCommand;

    use super::*;

    fn system() -> System {
        let site = demo::site(10);
        let mut sys = System::new(site.directory, site.rules, site.doors).unwrap();
        sys.objects_mut()
            .register(ObjectId(100), ObjectKind::AnalogSensor, "temp")
            .unwrap();
        sys
    }

    fn card(reader: envmon_core::model::ReaderId, n: u32) -> Uplink {
        Uplink::ReaderFrame {
            frame: ReaderFrame::CardRead {
                reader,
                card: demo::card(n),
            }
            .encode(),
        }
    }

    #[test]
    fn card_frame_unlocks_and_interlock_holds_rack() {
        let mut sys = system();
        let cfg = HostConfig::default();
        let at = demo::epoch() + TimeDelta::days(1);
        let now = Instant::now();

        let (_, rack) = apply(&mut sys, &cfg, &card(RACK_READER, 1), at, now).unwrap();
        assert_eq!(
            rack,
            vec![Downlink::Control(ControlCommand::ReaderFeedback {
                reader: RACK_READER,
                granted: false
            })]
        );
        let key = Uplink::KeyRequest {
            cabinet: KEY_CABINET,
            card: demo::card(1),
        };
        assert!(apply(&mut sys, &cfg, &key, at, now).unwrap().1.is_empty());

        let (_, room) = apply(&mut sys, &cfg, &card(ROOM_READER, 1), at, now).unwrap();
        assert_eq!(
            room.first(),
            Some(&Downlink::Control(ControlCommand::Unlock {
                door: ROOM_DOOR,
                duration_ms: 5000
            }))
        );
        let (_, released) = apply(&mut sys, &cfg, &key, at, now).unwrap();
        assert_eq!(
            released,
            vec![Downlink::Control(ControlCommand::ReleaseKey {
                cabinet: KEY_CABINET
            })]
        );
    }

    #[test]
    fn analog_values_are_classified() {
        let mut sys = system();
        let msg = Uplink::Analog {
            object: ObjectId(100),
            quantity: Quantity::TemperatureC,
            value: 35.0,
        };
        apply(
            &mut sys,
            &HostConfig::default(),
            &msg,
            demo::epoch(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(
            sys.objects().get(ObjectId(100)).unwrap().value,
            ObjectValue::Analog {
                value: 35.0,
                level: Level::Warning
            }
        );
    }

    #[test]
    fn bad_frames_and_unknown_objects_are_errors() {
        let mut sys = system();
        let cfg = HostConfig::default();
        let (at, now) = (demo::epoch(), Instant::now());
        let garbage = Uplink::ReaderFrame {
            frame: vec![1, 2, 3],
        };
        assert!(matches!(
            apply(&mut sys, &cfg, &garbage, at, now),
            Err(HostError::Frame(_))
        ));
        let unknown = Uplink::DigitalInput {
            object: ObjectId(999),
            on: true,
        };
        assert!(matches!(
            apply(&mut sys, &cfg, &unknown, at, now),
            Err(HostError::Object(_))
        ));
        let tamper = Uplink::ReaderFrame {
            frame: ReaderFrame::Tamper {
                reader: ROOM_READER,
            }
            .encode(),
        };
        assert_eq!(
            apply(&mut sys, &cfg, &tamper, at, now).unwrap().0,
            Notice::Tamper(ROOM_READER)
        );
    }
}
