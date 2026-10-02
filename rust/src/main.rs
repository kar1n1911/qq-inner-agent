//! Phase 1 配置入口；运行时配置与文件协议在独立模块中。
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use qq_inner_core::config;
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "qq-inner-core",
    version,
    about = "Rust core for qq-inner-agent"
)]
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
    /// Print a normalized summary with credentials redacted.
    Config,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = resolve_root(cli.root)?;

    match cli.command {
        Command::Config => {
            let loaded = config::load_config(&root)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&config::summary(&loaded.config))?
            );
        }
        Command::Selftest => {
            // 使用同一配置加载流程；SQLite 仍仅在内存中自检。
            let config = config::load_config(&root)?.raw;
            let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
            let sqlite: String = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;

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
