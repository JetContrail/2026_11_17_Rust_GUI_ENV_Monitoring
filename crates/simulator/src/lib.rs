//! Virtual hardware simulator.
//!
//! A [`Scenario`] is a list of [`Step`]s (card touches, door open/close, waits, sensor
//! changes, key requests). [`run_scenario`] starts a host on a loopback TCP port, connects the
//! [`VirtualSite`] to it over TLS 1.3 (mutual auth) or AES-256-GCM, and plays the steps with a
//! simulated clock so that long waits (door left open) take no real time.

pub mod devices;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{NaiveDateTime, TimeDelta};
use envmon_core::demo;
use envmon_core::door::DoorIndication;
use envmon_core::model::{CardId, DoorId, KeyCabinetId, ObjectId, ReaderId};
use envmon_core::objects::ObjectKind;
use envmon_core::system::{ControlCommand, System, SystemError};
use envmon_protocol::crypto::PresharedKey;
use envmon_protocol::device::{
    CardReader, DeviceError, ElectricLock, EnvironmentMonitor, KeyCabinet, Pdu,
};
use envmon_protocol::frame::ReaderFrame;
use envmon_protocol::host::{Clock, HostConfig, LinkScope, ServeError, serve_link};
use envmon_protocol::message::{Downlink, Quantity, Uplink};
use envmon_protocol::tls::{self, DevCa, TlsAcceptor, TlsConnector, TlsError};
use envmon_protocol::transport::{Link, LinkError};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};

use crate::devices::{
    VirtualEnvironment, VirtualKeyCabinet, VirtualLock, VirtualPdu, VirtualReader,
};

pub const TEMPERATURE: ObjectId = ObjectId(100);
pub const HUMIDITY: ObjectId = ObjectId(101);
pub const KEY_CABINET_OBJECT: ObjectId = ObjectId(200);
pub const RACK_OUTLET: ObjectId = ObjectId(300);

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error(transparent)]
    Crypto(#[from] envmon_protocol::crypto::CryptoError),
    #[error("host: {0}")]
    Serve(#[from] ServeError),
    #[error(transparent)]
    System(#[from] SystemError),
    #[error(transparent)]
    Object(#[from] envmon_core::objects::ObjectError),
    #[error("device: {0}")]
    Device(#[from] DeviceError),
    #[error("unknown virtual device {0}")]
    UnknownDevice(String),
    #[error("host task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Simulated wall clock and monotonic clock shared by the host and the virtual devices.
#[derive(Debug, Clone)]
pub struct SimClock(Arc<Mutex<(NaiveDateTime, Instant)>>);

impl SimClock {
    pub fn new(start: NaiveDateTime) -> Self {
        Self(Arc::new(Mutex::new((start, Instant::now()))))
    }

    pub fn advance(&self, by: Duration) {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        t.0 += TimeDelta::from_std(by).unwrap_or(TimeDelta::MAX);
        t.1 += by;
    }
}

impl Clock for SimClock {
    fn now(&self) -> (NaiveDateTime, Instant) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    CardTouch {
        reader: ReaderId,
        card: CardId,
    },
    OpenDoor {
        door: DoorId,
    },
    CloseDoor {
        door: DoorId,
    },
    /// Advance the simulated clock (a heartbeat lets the host run its timers).
    Wait {
        secs: u64,
    },
    Analog {
        object: ObjectId,
        value: f32,
    },
    DigitalInput {
        object: ObjectId,
        on: bool,
    },
    KeyRequest {
        cabinet: KeyCabinetId,
        card: CardId,
    },
    Outlet {
        object: ObjectId,
        on: bool,
        watts: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub steps: Vec<Step>,
}

/// The demo site scenario: interlock denial, room entry, door left open, temperature rise,
/// then rack and key-cabinet access.
pub fn demo_scenario() -> Scenario {
    let card = demo::card(1);
    Scenario {
        name: "demo".into(),
        steps: vec![
            Step::CardTouch {
                reader: demo::RACK_READER,
                card: card.clone(),
            },
            Step::KeyRequest {
                cabinet: demo::KEY_CABINET,
                card: card.clone(),
            },
            Step::CardTouch {
                reader: demo::ROOM_READER,
                card: card.clone(),
            },
            Step::OpenDoor {
                door: demo::ROOM_DOOR,
            },
            Step::Wait { secs: 5 },
            Step::Wait { secs: 6 },
            Step::Analog {
                object: TEMPERATURE,
                value: 24.5,
            },
            Step::Analog {
                object: TEMPERATURE,
                value: 41.0,
            },
            Step::Analog {
                object: HUMIDITY,
                value: 55.0,
            },
            Step::CloseDoor {
                door: demo::ROOM_DOOR,
            },
            Step::CardTouch {
                reader: demo::RACK_READER,
                card: card.clone(),
            },
            Step::KeyRequest {
                cabinet: demo::KEY_CABINET,
                card,
            },
            Step::Outlet {
                object: RACK_OUTLET,
                on: true,
                watts: 420.0,
            },
        ],
    }
}

/// Core system for the demo site with the simulator's sensor objects registered.
pub fn demo_system(users: u32) -> Result<System, SimError> {
    let site = demo::site(users);
    let mut sys = System::new(site.directory, site.rules, site.doors)?;
    let objects = sys.objects_mut();
    objects.register(
        TEMPERATURE,
        ObjectKind::AnalogSensor,
        "Server room temperature",
    )?;
    objects.register(HUMIDITY, ObjectKind::AnalogSensor, "Server room humidity")?;
    objects.register(KEY_CABINET_OBJECT, ObjectKind::KeyCabinet, "Key cabinet")?;
    objects.register(RACK_OUTLET, ObjectKind::PduOutlet, "Rack PDU outlet 1")?;
    Ok(sys)
}

/// All virtual devices of a site.
#[derive(Debug)]
pub struct VirtualSite {
    pub readers: BTreeMap<ReaderId, VirtualReader>,
    pub locks: BTreeMap<DoorId, VirtualLock>,
    pub environment: VirtualEnvironment,
    pub key_cabinets: BTreeMap<KeyCabinetId, (ObjectId, VirtualKeyCabinet)>,
    pub pdu: VirtualPdu,
}

impl VirtualSite {
    /// The host-side scope for a link carrying all of this site's devices.
    pub fn scope(&self) -> LinkScope {
        LinkScope {
            readers: self.readers.keys().copied().collect(),
            doors: self.locks.keys().copied().collect(),
            cabinets: self.key_cabinets.keys().copied().collect(),
            objects: self
                .environment
                .objects()
                .chain(self.pdu.objects())
                .chain(self.key_cabinets.values().map(|(object, _)| *object))
                .collect(),
        }
    }

    pub fn demo(now: Instant) -> Self {
        let mut environment = VirtualEnvironment::default();
        environment.add_analog(TEMPERATURE, Quantity::TemperatureC, 22.0);
        environment.add_analog(HUMIDITY, Quantity::HumidityPct, 45.0);
        let mut pdu = VirtualPdu::default();
        pdu.add_outlet(RACK_OUTLET, 0.0);
        Self {
            readers: [demo::ROOM_READER, demo::RACK_READER]
                .into_iter()
                .map(|r| (r, VirtualReader::new(r)))
                .collect(),
            locks: [demo::ROOM_DOOR, demo::RACK_DOOR]
                .into_iter()
                .map(|d| (d, VirtualLock::new(d, now)))
                .collect(),
            environment,
            key_cabinets: BTreeMap::from([(
                demo::KEY_CABINET,
                (
                    KEY_CABINET_OBJECT,
                    VirtualKeyCabinet::new(demo::KEY_CABINET, 8),
                ),
            )]),
            pdu,
        }
    }

    fn set_time(&mut self, now: Instant) {
        for lock in self.locks.values_mut() {
            lock.set_time(now);
        }
    }

    /// Performs `step` on the virtual devices and returns the messages they send.
    pub fn perform(&mut self, step: &Step, clock: &SimClock) -> Result<Vec<Uplink>, SimError> {
        self.set_time(clock.now().1);
        let unknown = |what: String| SimError::UnknownDevice(what);
        let messages = match step {
            Step::CardTouch { reader, card } => {
                let r = self
                    .readers
                    .get_mut(reader)
                    .ok_or_else(|| unknown(format!("reader {reader}")))?;
                r.touch(card.clone());
                let mut out = Vec::new();
                while let Some(card) = r.poll_card() {
                    let frame = ReaderFrame::CardRead {
                        reader: r.reader_id(),
                        card,
                    };
                    out.push(Uplink::ReaderFrame {
                        frame: frame.encode(),
                    });
                }
                out
            }
            Step::OpenDoor { door } | Step::CloseDoor { door } => {
                let open = matches!(step, Step::OpenDoor { .. });
                let lock = self
                    .locks
                    .get_mut(door)
                    .ok_or_else(|| unknown(format!("door {door}")))?;
                lock.set_open(open);
                vec![Uplink::DoorSensor {
                    door: *door,
                    open: lock.door_open(),
                }]
            }
            Step::Wait { secs } => {
                clock.advance(Duration::from_secs(*secs));
                self.set_time(clock.now().1);
                let mut out: Vec<Uplink> = self
                    .locks
                    .values()
                    .map(|l| Uplink::LockStatus {
                        door: l.door_id(),
                        state: l.lock_state(),
                    })
                    .collect();
                out.push(Uplink::Heartbeat);
                out
            }
            Step::Analog { object, value } => {
                let quantity = self.environment.set_analog(*object, *value)?;
                vec![Uplink::Analog {
                    object: *object,
                    quantity,
                    value: *value,
                }]
            }
            Step::DigitalInput { object, on } => {
                self.environment.set_input(*object, *on);
                vec![Uplink::DigitalInput {
                    object: *object,
                    on: *on,
                }]
            }
            Step::KeyRequest { cabinet, card } => {
                vec![Uplink::KeyRequest {
                    cabinet: *cabinet,
                    card: card.clone(),
                }]
            }
            Step::Outlet { object, on, watts } => {
                if !self.pdu.has(*object) {
                    return Err(unknown(format!("outlet {object}")));
                }
                self.pdu.set_load(*object, *on, *watts);
                let reading = self
                    .pdu
                    .outlets()?
                    .into_iter()
                    .find(|o| o.object == *object);
                reading
                    .map(|o| Uplink::Outlet {
                        object: o.object,
                        on: o.on,
                        watts: o.watts,
                    })
                    .into_iter()
                    .collect()
            }
        };
        Ok(messages)
    }

    /// Executes host commands on the virtual devices; returns status messages they report.
    pub fn execute(&mut self, downlink: &Downlink) -> Result<Vec<Uplink>, SimError> {
        let unknown = |what: String| SimError::UnknownDevice(what);
        Ok(match downlink {
            Downlink::Control(ControlCommand::Unlock { door, duration_ms }) => {
                let lock = self
                    .locks
                    .get_mut(door)
                    .ok_or_else(|| unknown(format!("door {door}")))?;
                lock.unlock(Duration::from_millis(u64::from(*duration_ms)))?;
                vec![]
            }
            Downlink::Control(ControlCommand::ReaderFeedback { reader, granted }) => {
                let r = self
                    .readers
                    .get_mut(reader)
                    .ok_or_else(|| unknown(format!("reader {reader}")))?;
                r.feedback(*granted)?;
                vec![]
            }
            Downlink::Control(ControlCommand::ReleaseKey { cabinet }) => {
                let (object, c) = self
                    .key_cabinets
                    .get_mut(cabinet)
                    .ok_or_else(|| unknown(format!("key cabinet {cabinet}")))?;
                c.release_key()?;
                vec![Uplink::KeyCabinet {
                    object: *object,
                    keys_out: c.keys_out(),
                }]
            }
            Downlink::SetOutput { object, on } => {
                if self.pdu.has(*object) {
                    self.pdu.set_outlet(*object, *on)?;
                } else {
                    self.environment.set_output(*object, *on)?;
                }
                vec![]
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    /// TLS 1.3 with mutual certificate authentication.
    Tls,
    /// AES-256-GCM records keyed from a pre-shared key.
    Aes,
}

/// What happened during one step.
#[derive(Debug, Clone)]
pub struct StepReport {
    pub elapsed_secs: i64,
    pub step: Step,
    pub uplinks: Vec<Uplink>,
    pub downlinks: Vec<Downlink>,
    pub doors: Vec<(DoorId, DoorIndication)>,
    pub active_alarms: usize,
}

/// Plays `scenario` against `system` over a loopback connection.
pub async fn run_scenario(
    system: Arc<Mutex<System>>,
    site: &mut VirtualSite,
    clock: &SimClock,
    scenario: &Scenario,
    transport: Transport,
) -> Result<Vec<StepReport>, SimError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let host_clock: Arc<dyn Clock> = Arc::new(clock.clone());
    let config = HostConfig {
        scope: site.scope(),
        ..HostConfig::default()
    };
    let host_system = Arc::clone(&system);

    let reports = match transport {
        Transport::Tls => {
            let ca = DevCa::generate("envmon simulator CA")?;
            let server =
                tls::mutual_server_config(&ca.issue_server(&["localhost"])?, &[ca.certificate()])?;
            let client =
                tls::client_config(&[ca.certificate()], Some(&ca.issue_client("simulator")?))?;
            let host = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await?;
                let tls = TlsAcceptor::from(server).accept(tcp).await?;
                serve_link(Link::tls(tls), host_system, config, host_clock).await?;
                Ok::<_, SimError>(())
            });
            let tcp = TcpStream::connect(addr).await?;
            let stream = TlsConnector::from(client)
                .connect(tls::server_name("localhost")?, tcp)
                .await?;
            let reports = play(Link::tls(stream), &system, site, clock, scenario).await?;
            host.await??;
            reports
        }
        Transport::Aes => {
            let psk = PresharedKey::generate()?;
            let host_psk = psk.clone();
            let host = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await?;
                let link = Link::aes_host(tcp, &host_psk).await?;
                serve_link(link, host_system, config, host_clock).await?;
                Ok::<_, SimError>(())
            });
            let link = Link::aes_device(TcpStream::connect(addr).await?, &psk).await?;
            let reports = play(link, &system, site, clock, scenario).await?;
            host.await??;
            reports
        }
    };
    Ok(reports)
}

async fn play<S>(
    mut link: Link<S>,
    system: &Mutex<System>,
    site: &mut VirtualSite,
    clock: &SimClock,
    scenario: &Scenario,
) -> Result<Vec<StepReport>, SimError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let start = clock.now().0;
    let mut reports = Vec::with_capacity(scenario.steps.len());
    for step in &scenario.steps {
        let mut pending = site.perform(step, clock)?;
        let mut uplinks = Vec::new();
        let mut downlinks = Vec::new();
        while !pending.is_empty() {
            let mut next = Vec::new();
            for uplink in pending {
                link.send(&uplink).await?;
                let reply: Vec<Downlink> = link.recv().await?;
                for d in &reply {
                    next.extend(site.execute(d)?);
                }
                uplinks.push(uplink);
                downlinks.extend(reply);
            }
            pending = next;
        }
        let sys = system.lock().unwrap_or_else(PoisonError::into_inner);
        let doors = sys
            .door_configs()
            .filter_map(|c| sys.door_indication(c.id).map(|i| (c.id, i)))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect();
        reports.push(StepReport {
            elapsed_secs: (clock.now().0 - start).num_seconds(),
            step: step.clone(),
            uplinks,
            downlinks,
            doors,
            active_alarms: sys.active_alarms().count(),
        });
    }
    Ok(reports)
}
