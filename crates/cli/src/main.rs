//! Headless command line for the agent runtime: list models and MCP connections, run a
//! prompt (optionally with an agent), and manage agent schedules. Runs go through
//! `agent_core::agent::launcher::launch`, the same path the desktop app uses.
//!
//! The CLI shares the data root, provider configs and keychain secrets with the desktop app
//! and may run while it is open: it opens the database through `Db::open_root` (process lock)
//! and loads `providers.json`, `mcp.json` and `settings.json` read-only.

mod args;
mod list;
mod run;
mod schedule;
mod term;

use std::process::ExitCode;
use std::sync::Arc;

use agent_core::config::{data_dir_env_var, log_env_var, product};
use agent_core::secrets::{MemorySecretStore, NativeSecretStore, SecretStore};
use tracing_subscriber::EnvFilter;

use args::{Arg, Args};

/// Exit codes: a run that completed (or a command that succeeded) exits 0.
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_CANCELLED: u8 = 3;
/// A second Ctrl-C quit without waiting for the cancellation to be stored.
pub const EXIT_INTERRUPTED: u8 = 130;

#[derive(Debug)]
pub enum CliError {
    /// Bad arguments or a request that can't be served as asked; exit 2.
    Usage(String),
    /// Exit 1.
    Failed(String),
    /// Already reported (e.g. the run's own error event); exit with this code.
    Exit(u8),
}

impl From<agent_core::Error> for CliError {
    fn from(e: agent_core::Error) -> Self {
        use agent_core::Error as E;
        match e {
            E::InvalidInput(m) | E::NotFound(m) => CliError::Usage(m),
            E::RunBusy => CliError::Failed(
                "a reply is still being generated in this chat — wait for it to finish or stop \
                 it in the app"
                    .into(),
            ),
            e => CliError::Failed(e.to_string()),
        }
    }
}

impl CliError {
    fn report(self) -> u8 {
        let bin = product().slug;
        match self {
            CliError::Usage(m) => {
                term::note(&format!("error: {m}\nRun `{bin} help` for usage."));
                EXIT_USAGE
            }
            CliError::Failed(m) => {
                term::note(&format!("error: {m}"));
                EXIT_FAILED
            }
            CliError::Exit(code) => code,
        }
    }
}

pub type Result<T> = std::result::Result<T, CliError>;

fn usage_text() -> String {
    let bin = product().slug;
    let name = product().display_name;
    format!(
        "{name} {version} — command line

Usage:
  {bin} models [--json]
      List configured providers and their models.
  {bin} mcp list [--tools] [--json]
      List MCP connections; --tools adds each server's last known tools.
  {bin} run [options] <prompt>
      Run one prompt and print the reply. Without <prompt>, piped stdin is read.
      --model <provider>/<model>   Provider name or id, then the model (plain chats)
      --agent <name>               Run with an agent (its models, tools and permissions)
      --chat <id>                  Continue a chat (the id is printed for new chats)
      --mcp <server>               Use only these MCP servers (repeatable)
      --tool <server>/<tool>       Use only these tools of a server (repeatable)
      --no-mcp                     No MCP servers for this chat
      --json                       Print the run's events as JSON lines
  {bin} schedule add --agent <name> (--at <time> | --every <interval>) [--skip-missed] <prompt>
      --at \"2026-12-01 09:00\" or \"09:00\" (local time)
      --every 30m | 2h | 1d | weekdays@09:00 | mon,thu@18:30 | daily@07:15
  {bin} schedule list [--agent <name>] [--json]
  {bin} schedule remove <id>
  {bin} help | --help | -h        This text (also after any command)
  {bin} version | --version | -V

Schedules run while the desktop app is open.
Exit codes: 0 completed, {EXIT_FAILED} failed, {EXIT_USAGE} usage error, {EXIT_CANCELLED} cancelled.
Environment: {data_dir} (data folder), {log} (log filter for stderr, e.g. debug).
",
        version = product().version,
        data_dir = data_dir_env_var(),
        log = log_env_var(),
    )
}

/// The runtime's warnings (a write that failed, a setting that couldn't be read) go to
/// stderr instead of nowhere; the app's log variable takes a `tracing` filter for more.
/// Secrets never reach tracing fields; see `agent_core::secrets`.
fn init_tracing() {
    let filter = EnvFilter::try_from_env(log_env_var()).unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

fn dispatch(args: &mut Args) -> Result<()> {
    let Some(command) = args.word() else {
        return term::out(&usage_text());
    };
    if args.wants_help() {
        return term::out(&usage_text());
    }
    match command.as_str() {
        "models" => list::models(args),
        "mcp" => match args.word().as_deref() {
            Some("list") => list::mcp(args),
            Some(other) => Err(CliError::Usage(format!("unknown command `mcp {other}`"))),
            None => Err(CliError::Usage("`mcp` needs a command: list".into())),
        },
        "run" => run::run(run::parse(args)?),
        "schedule" => schedule::schedule(args),
        "help" | "--help" | "-h" => term::out(&usage_text()),
        "--version" | "-V" | "version" => {
            term::out(&format!("{} {}\n", product().slug, product().version))
        }
        other => Err(CliError::Usage(format!("unknown command `{other}`"))),
    }
}

/// Rejects anything left after a command that takes no arguments.
pub fn no_more(args: &mut Args) -> Result<()> {
    match args.next_arg() {
        None => Ok(()),
        Some(Arg::Flag(f, _)) => Err(args::unknown(&f)),
        Some(Arg::Positional(p)) => Err(CliError::Usage(format!("unexpected argument `{p}`"))),
    }
}

/// The OS keychain, like the app; nothing is read from it until a run needs a key.
pub fn secret_store() -> Arc<dyn SecretStore> {
    match NativeSecretStore::new() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            term::note(&format!(
                "warning: the OS credential store is unavailable ({e}); stored keys can't be used"
            ));
            Arc::new(MemorySecretStore::default())
        }
    }
}

pub fn http_client() -> Result<agent_core::http::Client> {
    agent_core::http::client().map_err(|e| CliError::Failed(e.to_string()))
}

fn main() -> ExitCode {
    init_tracing();
    let mut args = Args::new(std::env::args().skip(1));
    let code = match dispatch(&mut args) {
        Ok(()) => 0,
        Err(e) => e.report(),
    };
    ExitCode::from(code)
}
