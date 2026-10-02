//! qq-inner-core — the Rust core for qq-inner-agent.
//!
//! Phase 0 skeleton. It resolves the repository root, loads `config.json`, opens
//! an in-memory SQLite database, and reports what it found. This exists to prove
//! the toolchain and dependency set on the deployment host before the real
//! modules land.
//!
//! Roadmap (see `docs/rust-port/ARCHITECTURE.md`):
//!   Phase 1  config + store + logging/status.json
//!   Phase 2  onebot transport
//!   Phase 3  provider adapters
//!   Phase 4  engine + policy + sending + activity + orientation
//!   Phase 5  memory + ranking + expression + learning
//!   Phase 6  control socket + Node dashboard as a thin client

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "qq-inner-core", version, about = "Rust core for qq-inner-agent")]
struct Cli {
    /// Repository root. Defaults to $QQ_INNER_ROOT, then walks up from the
    /// current directory looking for `config.example.json`.
    #[arg(long, global = true, env = "QQ_INNER_ROOT")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Probe the repository root, configuration, and SQLite, then exit.
    Selftest,
}

fn resolve_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        return Ok(root);
    }
    let cwd = std::env::current_dir().context("read current directory")?;
    for dir in cwd.ancestors() {
        if dir.join("config.example.json").is_file() {
            return Ok(dir.to_path_buf());
        }
    }
    Ok(cwd)
}

fn load_config(root: &Path) -> Result<Value> {
    let path = root.join("config.json");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = resolve_root(cli.root)?;

    match cli.command {
        Command::Selftest => {
            let config = load_config(&root)?;
            let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
            let sqlite: String =
                conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;

            let field = |pointer: &str| {
                config
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .unwrap_or("(unset)")
                    .to_string()
            };

            println!("core version   = {}", env!("CARGO_PKG_VERSION"));
            println!("root           = {}", root.display());
            println!("config.json    = ok");
            println!("sqlite         = {sqlite}");
            println!("provider.kind  = {}", field("/provider/kind"));
            println!("provider.model = {}", field("/provider/model"));
            println!("selftest ok");
        }
    }

    Ok(())
}
