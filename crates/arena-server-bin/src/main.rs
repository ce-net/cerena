//! `arena-server` — run one Cerena zone/coordinator authority on this CE node.
//!
//! This binary is a thin shell around [`arena_server::ArenaServer`]: parse flags into a
//! [`ServerConfig`], connect to the local CE node, and run the fixed-tick actor loop until
//! ctrl_c. All the real work lives in the `arena-server` library crate.
//!
//! ## Usage
//!
//! ```text
//! arena-server --session <id> [--api-port N] [--p2p-port N] [--data-dir DIR]
//!              [--coordinator] [--bootstrap URL] [--map ID]
//!              [--e2e-insecure] [--e2e-admin] [--e2e-cheat] [--tick-hz N]
//! ```
//!
//! It connects to the **local CE node's** HTTP API at `http://127.0.0.1:<api-port>` (the
//! node must already be running, e.g. `ce start`); `--p2p-port` and `--bootstrap` concern the
//! CE node itself and are accepted for command-line parity but not used here.

use std::process::ExitCode;

use arena_server::{ArenaServer, ServerConfig};

use arena_protocol::auth::SessionId;
use arena_protocol::world::MapId;

/// Parsed command-line options.
struct Args {
    session: String,
    api_port: u16,
    data_dir: Option<String>,
    coordinator: bool,
    bootstrap: Option<String>,
    map: String,
    e2e_insecure: bool,
    e2e_admin: bool,
    e2e_cheat: bool,
    tick_hz: u32,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            session: "cerena-dev".to_string(),
            api_port: 8844,
            data_dir: None,
            coordinator: false,
            bootstrap: None,
            map: "cerena-overworld".to_string(),
            e2e_insecure: false,
            e2e_admin: false,
            e2e_cheat: false,
            tick_hz: arena_protocol::TICK_HZ,
        }
    }
}

/// Minimal hand-rolled flag parsing (no clap dependency by design). Unknown flags are an
/// error so a typo never silently runs the wrong configuration.
fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        // Helper to pull the value for a `--flag value` pair.
        let mut value = || it.next().ok_or_else(|| format!("flag {flag} requires a value"));
        match flag.as_str() {
            "--session" => args.session = value()?,
            "--api-port" => {
                args.api_port = value()?
                    .parse()
                    .map_err(|e| format!("--api-port: {e}"))?
            }
            "--p2p-port" => {
                // CE-node concern; accepted for parity, ignored here.
                let _ = value()?;
            }
            "--data-dir" => args.data_dir = Some(value()?),
            "--coordinator" => args.coordinator = true,
            "--bootstrap" => args.bootstrap = Some(value()?),
            "--map" => args.map = value()?,
            "--e2e-insecure" => args.e2e_insecure = true,
            "--e2e-admin" => args.e2e_admin = true,
            "--e2e-cheat" => args.e2e_cheat = true,
            "--tick-hz" => {
                args.tick_hz = value()?.parse().map_err(|e| format!("--tick-hz: {e}"))?
            }
            "-h" | "--help" => return Err("help".to_string()),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(args)
}

/// Read the local CE node's API token from `<data_dir>/api.token` when a custom data dir was
/// given (the node writes it there); otherwise `None`, letting `ce_rs` discovery take over.
fn token_from_data_dir(data_dir: &Option<String>) -> Option<String> {
    let dir = data_dir.as_ref()?;
    std::fs::read_to_string(std::path::Path::new(dir).join("api.token"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

const USAGE: &str = "\
arena-server — run a Cerena zone/coordinator authority on this CE node

USAGE:
    arena-server --session <id> [options]

OPTIONS:
    --session <id>      session/match id (default: cerena-dev)
    --api-port <n>      local CE node HTTP API port (default: 8844)
    --p2p-port <n>      CE node p2p port (accepted for parity; ignored here)
    --data-dir <dir>    CE node data dir (used to read its api.token)
    --coordinator       run the session coordinator role on this node
    --bootstrap <url>   CE node bootstrap (accepted for parity; ignored here)
    --map <id>          map id reported to clients (default: cerena-overworld)
    --e2e-insecure      TEST ONLY: relax session-ticket verification
    --e2e-admin         TEST ONLY: enable the test admin surface
    --e2e-cheat         TEST ONLY: act as a malicious authority (wrong state hash)
    --tick-hz <n>       simulation tick rate (default: 64)
";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) if e == "help" => {
            eprint!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("arena-server: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let api_url = format!("http://127.0.0.1:{}", args.api_port);
    if let Some(bootstrap) = &args.bootstrap {
        // The CE node handles its own bootstrap; we only note it for operator clarity.
        eprintln!("arena-server: note: --bootstrap {bootstrap} is a CE-node concern and is ignored here");
    }

    let config = ServerConfig {
        session: SessionId(args.session.clone()),
        api_url: api_url.clone(),
        node_token: token_from_data_dir(&args.data_dir),
        coordinator: args.coordinator,
        map: MapId(args.map.clone()),
        e2e_insecure: args.e2e_insecure,
        e2e_admin: args.e2e_admin,
        e2e_cheat: args.e2e_cheat,
        tick_hz: if args.tick_hz == 0 { arena_protocol::TICK_HZ } else { args.tick_hz },
        // Proximity-replication defaults; not exposed as flags (sane fleet-wide values).
        replication_factor: 3,
        replication_interval_ticks: 64,
    };

    // Build the server (this is where an unreachable CE node fails with a clear message).
    let server = match ArenaServer::new(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("arena-server: failed to start: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let role = if args.coordinator { "coordinator+authority" } else { "authority" };
    eprintln!(
        "arena-server: up\n  node:    {}\n  session: {}\n  role:    {}\n  api:     {}\n  tick:    {} Hz",
        server.node_id(),
        args.session,
        role,
        api_url,
        args.tick_hz,
    );

    match server.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("arena-server: exited with error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
