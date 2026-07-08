//! Forge CLI — headless driver for a running Forge OS API server.
//!
//! ```text
//! forge health
//! forge missions list
//! forge missions get <ID>
//! forge missions cancel <ID>
//! forge run "title of the mission" [--wait]
//! forge events [--mission ID] [--since N] [--follow]
//! forge chat "prompt"
//! forge skill bundle <SKILL_DIR> --out <FILE> --key <ED25519_KEY>
//! forge skill verify <BUNDLE> --pubkey <PUB>
//! forge skill install <BUNDLE> [--dest <DIR>]
//! forge keygen --out <FILE>
//! ```
//!
//! Global flags:
//!   --url    (env: FORGE_API_URL, default http://127.0.0.1:7823)
//!   --token  (env: FORGE_API_TOKEN)
//!   --json   machine-readable output (default is human-friendly)

use anyhow::Result;
use clap::{Parser, Subcommand};

mod bundle;
mod client;
mod cmd;
mod render;

#[derive(Parser)]
#[command(name = "forge", version, about = "Headless CLI for Forge OS.")]
#[command(propagate_version = true, disable_help_subcommand = true)]
struct Cli {
    /// Base URL of the Forge API server.
    #[arg(long, env = "FORGE_API_URL", default_value = "http://127.0.0.1:7823", global = true)]
    url: String,

    /// Bearer token. Read from FORGE_API_TOKEN by default.
    #[arg(long, env = "FORGE_API_TOKEN", default_value = "", global = true)]
    token: String,

    /// Emit raw JSON (default: human-friendly).
    #[arg(long, global = true)]
    json: bool,

    /// HTTP timeout in seconds for one-shot calls (not events / wait).
    #[arg(long, default_value_t = 30, global = true)]
    timeout: u64,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ping /health.
    Health,

    /// Mission operations.
    Missions {
        #[command(subcommand)]
        op: cmd::missions::Op,
    },

    /// Short-hand for `missions run <title> --wait`.
    Run {
        /// Mission title.
        title: String,
        /// Optional description body. Defaults to the title.
        #[arg(long)]
        description: Option<String>,
        /// Block until the mission reaches a terminal state and print the summary.
        #[arg(long)]
        wait: bool,
        /// Also stream events while waiting.
        #[arg(long)]
        stream: bool,
    },

    /// SSE-stream events (like `tail -f`).
    Events {
        /// Skip everything with seq ≤ this value.
        #[arg(long, default_value_t = 0)]
        since: u64,
        /// Filter to a single mission id (raw UUID).
        #[arg(long)]
        mission: Option<String>,
        /// Keep the connection open (default true unless --once).
        #[arg(long, conflicts_with = "once")]
        follow: bool,
        /// Grab whatever is buffered and exit.
        #[arg(long)]
        once: bool,
    },

    /// Call the OpenAI-compat chat completion shim.
    Chat {
        /// User message.
        prompt: String,
        /// System prompt to prepend.
        #[arg(long)]
        system: Option<String>,
        /// Model name (server ignores this today; passed through for logging).
        #[arg(long, default_value = "forge-mission")]
        model: String,
    },

    /// Signed skill bundles for sharing.
    Skill {
        #[command(subcommand)]
        op: cmd::skill::Op,
    },

    /// Generate an ed25519 keypair for bundle signing.
    Keygen {
        /// Where to write the private key (base64-encoded ed25519 secret).
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Only wire logging if the caller opted in; the CLI is meant to be quiet by default.
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }

    let cli = Cli::parse();
    let client = client::ApiClient::new(&cli.url, &cli.token, cli.timeout);

    match cli.command {
        Cmd::Health => cmd::health::run(&client, cli.json).await,
        Cmd::Missions { op } => cmd::missions::dispatch(&client, cli.json, op).await,
        Cmd::Run { title, description, wait, stream } => {
            cmd::missions::run_shortcut(&client, cli.json, title, description, wait, stream).await
        }
        Cmd::Events { since, mission, follow, once } => {
            let follow = if once { false } else { follow || !once };
            cmd::events::run(&client, cli.json, since, mission, follow).await
        }
        Cmd::Chat { prompt, system, model } => {
            cmd::chat::run(&client, cli.json, model, system, prompt).await
        }
        Cmd::Skill { op } => cmd::skill::dispatch(cli.json, op).await,
        Cmd::Keygen { out } => bundle::keygen(&out),
    }
}
