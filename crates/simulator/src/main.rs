//! `envmon-sim`: virtual hardware for running the system without devices.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use clap::{Parser, Subcommand};
use envmon_core::demo;
use envmon_core::model::{CardId, ReaderId};
use envmon_protocol::crypto::PresharedKey;
use envmon_protocol::frame::ReaderFrame;
use envmon_simulator::{
    Scenario, SimClock, Transport, VirtualSite, demo_scenario, demo_system, run_scenario,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Virtual card readers, doors, sensors, key cabinets and PDUs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a scenario against an in-process host over an encrypted loopback link.
    Run {
        #[arg(long, value_enum, default_value_t = Transport::Tls)]
        transport: Transport,
        /// Scenario JSON (default: the built-in demo scenario).
        #[arg(long)]
        scenario: Option<PathBuf>,
        /// Number of registered users.
        #[arg(long, default_value_t = 1_000)]
        users: u32,
    },
    /// Print the built-in demo scenario as JSON (a template for custom scenarios).
    Scenario,
    /// Encode a card-read frame and print it as hex.
    Frame {
        #[arg(long)]
        reader: u32,
        /// Card ID in hex.
        #[arg(long)]
        card: CardId,
    },
    /// Generate a random AES-256-GCM pre-shared key (hex).
    Keygen,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

async fn run(transport: Transport, scenario: Option<PathBuf>, users: u32) -> Result<(), String> {
    let scenario: Scenario = match scenario {
        Some(path) => {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
        }
        None => demo_scenario(),
    };
    let clock = SimClock::new(demo::epoch() + chrono::TimeDelta::days(1));
    let system = Arc::new(Mutex::new(demo_system(users).map_err(|e| e.to_string())?));
    let mut site = VirtualSite::demo(Instant::now());
    println!(
        "scenario {:?} over {transport:?}, {users} users",
        scenario.name
    );
    let reports = run_scenario(Arc::clone(&system), &mut site, &clock, &scenario, transport)
        .await
        .map_err(|e| e.to_string())?;
    for r in &reports {
        println!(
            "[t+{:>3}s] {}",
            r.elapsed_secs,
            serde_json::to_string(&r.step).unwrap_or_default()
        );
        for d in &r.downlinks {
            println!(
                "          -> {}",
                serde_json::to_string(d).unwrap_or_default()
            );
        }
        for (door, ind) in &r.doors {
            let flags = match (ind.sound, ind.blink) {
                (true, true) => " [ALARM: sound + red blink]",
                _ => "",
            };
            println!(
                "          door {door}: {:?} / {:?}{flags}",
                ind.door, ind.lock
            );
        }
    }
    let sys = system.lock().unwrap_or_else(PoisonError::into_inner);
    println!("alarms:");
    for a in sys.alarms() {
        println!("  {}", serde_json::to_string(a).unwrap_or_default());
    }
    println!("objects:");
    for o in sys.objects().iter() {
        println!(
            "  {} {:?}: {}",
            o.id,
            o.name,
            serde_json::to_string(&o.value).unwrap_or_default()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run {
            transport,
            scenario,
            users,
        } => run(transport, scenario, users).await,
        Command::Scenario => serde_json::to_string_pretty(&demo_scenario())
            .map(|json| println!("{json}"))
            .map_err(|e| e.to_string()),
        Command::Frame { reader, card } => {
            println!(
                "{}",
                hex(&ReaderFrame::CardRead {
                    reader: ReaderId(reader),
                    card
                }
                .encode())
            );
            Ok(())
        }
        Command::Keygen => PresharedKey::generate()
            .map(|key| println!("{}", key.to_hex()))
            .map_err(|e| e.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
