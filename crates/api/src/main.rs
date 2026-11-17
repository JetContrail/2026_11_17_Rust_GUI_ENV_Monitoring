//! `envmon-api`: standalone HTTPS WebAPI server over the demo site (for integration testing).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Parser;
use envmon_api::auth::{TokenStore, generate_token};
use envmon_api::{ApiState, Role, router, server};
use envmon_core::demo;
use envmon_core::store::Store;
use envmon_core::system::System;
use envmon_protocol::host::{SystemClock, run_timers};
use envmon_protocol::tls::{self, DevCa, Identity};

#[derive(Debug, Parser)]
#[command(version, about = "HTTPS REST WebAPI (TLS 1.3, bearer-token RBAC)")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:8443")]
    listen: SocketAddr,
    /// Server certificate chain (PEM). Without --cert/--key a development CA is generated.
    #[arg(long, requires = "key")]
    cert: Option<PathBuf>,
    /// Server private key (PKCS#8 PEM).
    #[arg(long, requires = "cert")]
    key: Option<PathBuf>,
    /// Where to write the generated development CA certificate (PEM).
    #[arg(long, default_value = "envmon-dev-ca.pem")]
    dev_ca_out: PathBuf,
    /// JSON array of {"name","role","token_sha256"} entries.
    #[arg(long)]
    tokens: Option<PathBuf>,
    /// SQLite database for persistent access history.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Number of demo users.
    #[arg(long, default_value_t = 1_000)]
    users: u32,
}

async fn run(cli: Cli) -> Result<(), String> {
    let identity = match (&cli.cert, &cli.key) {
        (Some(cert), Some(key)) => {
            Identity::from_pem_files(cert, key).map_err(|e| e.to_string())?
        }
        _ => {
            let ca = DevCa::generate("envmon dev CA").map_err(|e| e.to_string())?;
            std::fs::write(&cli.dev_ca_out, ca.certificate_pem()).map_err(|e| e.to_string())?;
            eprintln!("development CA written to {}", cli.dev_ca_out.display());
            ca.issue_server(&["localhost", "127.0.0.1"])
                .map_err(|e| e.to_string())?
        }
    };
    let tokens = match &cli.tokens {
        Some(path) => TokenStore::load(path).map_err(|e| e.to_string())?,
        None => {
            let token = generate_token().map_err(|e| e.to_string())?;
            eprintln!("no --tokens given; one-time admin token: {token}");
            let mut store = TokenStore::new();
            store.add("admin", Role::Admin, &token);
            store
        }
    };
    let site = demo::site(cli.users);
    let system = match &cli.db {
        Some(db) => {
            let mut store = Store::open(db).map_err(|e| e.to_string())?;
            // Schedules and groups are site configuration and always written, so users saved
            // without them still load; saved users (with any permission changes) win over the
            // demo seed.
            for schedule in site.directory.schedules() {
                store.save_schedule(schedule).map_err(|e| e.to_string())?;
            }
            for group in site.directory.groups() {
                store.save_group(group).map_err(|e| e.to_string())?;
            }
            let directory = if store.user_count().map_err(|e| e.to_string())? > 0 {
                store.load_directory().map_err(|e| e.to_string())?
            } else {
                demo::save(&site.directory, &mut store).map_err(|e| e.to_string())?;
                site.directory
            };
            System::new(directory, site.rules, site.doors)
                .and_then(|s| s.with_store(store))
                .map_err(|e| e.to_string())?
        }
        None => System::new(site.directory, site.rules, site.doors).map_err(|e| e.to_string())?,
    };
    let system = Arc::new(Mutex::new(system));
    tokio::spawn(run_timers(
        Arc::clone(&system),
        Arc::new(SystemClock),
        std::time::Duration::from_millis(200),
    ));
    let state = ApiState::new(system, tokens, Arc::new(SystemClock));
    let config = tls::https_server_config(&identity).map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("listening on https://{}", cli.listen);
    server::serve_https(listener, config, router(state))
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
