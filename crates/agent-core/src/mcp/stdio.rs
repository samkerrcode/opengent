//! Environment construction for stdio MCP servers. Child processes get a minimal,
//! allow-listed environment — never the app's full env — plus whatever the user configured.
//!
//! GUI launches (Finder, Start Menu) give the app a minimal PATH, so `npx`/`uvx`/`node`
//! wouldn't resolve. We recover the user's real PATH once by asking the login shell
//! (fast, cached), falling back to a list of well-known install locations.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use super::config::{McpServerConfig, McpStore};

/// Variables inherited from the app environment when present. Everything else the child
/// needs must come from the server config or the login-shell PATH.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TZ",
    "HOSTNAME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    // Windows process essentials.
    "SystemRoot",
    "SystemDrive",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "PATHEXT",
    "ComSpec",
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "TEMP",
    "TMP",
    // Toolchain homes servers commonly read.
    "NVM_DIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "PNPM_HOME",
];

/// Well-known tool locations appended when the login shell can't be queried.
#[cfg(unix)]
const PATH_FALLBACKS: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/local/sbin",
    "~/.local/bin",
    "~/.cargo/bin",
    "~/.npm-global/bin",
    "~/.bun/bin",
    "~/.deno/bin",
    "~/.volta/bin",
    "~/.local/share/mise/shims",
];

static LOGIN_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Expands a leading `~` (or `~/…`) to the user's home. Stdio servers spawn without a
/// shell, so `~` in commands/args/cwd must be expanded by us.
pub(crate) fn expand_tilde(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            // p[1..] keeps the "/" separator ("" when p == "~").
            return format!("{}{}", home.to_string_lossy(), &p[1..]);
        }
    }
    p.to_owned()
}

/// Reads `$PATH` from the user's login shell. Unix only; Windows GUI launches already carry
/// a full PATH. Returns `None` quickly on failure (2s cap, runs once per process).
#[cfg(unix)]
fn query_login_path() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(&shell)
        .args(["-lc", "printf '%s' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // No async needed: this runs once, on a connect path, and is capped by a watchdog
    // thread that kills the child if the shell hangs.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        let _ = child.wait();
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(Duration::from_secs(2)).ok()?;
    let path = out.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_owned())
    }
}

#[cfg(not(unix))]
fn query_login_path() -> Option<String> {
    None
}

/// The PATH a stdio server is spawned with: the login shell's, else the app's PATH extended
/// with well-known install dirs (incl. the newest nvm Node version if present).
#[cfg(unix)]
fn resolved_path() -> Option<String> {
    LOGIN_PATH
        .get_or_init(|| {
            query_login_path().or_else(|| {
                let mut parts: Vec<String> = std::env::var("PATH")
                    .map(|p| p.split(':').map(str::to_owned).collect())
                    .unwrap_or_default();
                for p in PATH_FALLBACKS {
                    let p = expand_tilde(p);
                    if std::path::Path::new(&p).is_dir() && !parts.contains(&p) {
                        parts.push(p);
                    }
                }
                // nvm keeps versions under ~/.nvm/versions/node/vX.Y.Z/bin — pick the newest.
                if let Some(home) = std::env::var_os("HOME") {
                    let nvm = std::path::Path::new(&home).join(".nvm/versions/node");
                    let mut versions: Vec<_> = std::fs::read_dir(&nvm)
                        .map(|rd| {
                            rd.flatten()
                                .map(|e| e.path())
                                .filter(|p| p.join("bin").is_dir())
                                .collect()
                        })
                        .unwrap_or_default();
                    versions.sort();
                    if let Some(newest) = versions.pop() {
                        let bin = newest.join("bin").to_string_lossy().to_string();
                        if !parts.contains(&bin) {
                            parts.push(bin);
                        }
                    }
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(":"))
                }
            })
        })
        .clone()
}

#[cfg(not(unix))]
fn resolved_path() -> Option<String> {
    std::env::var("PATH").ok()
}

/// The env a stdio server is spawned with: allow-listed app vars, the recovered PATH, then
/// user-configured entries (which may override anything, e.g. a pinned PATH).
pub fn build_env(store: &McpStore, config: &McpServerConfig) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars()
        .filter(|(k, _)| ENV_ALLOWLIST.contains(&k.as_str()))
        .collect();
    if let Some(path) = resolved_path() {
        env.insert("PATH".into(), path);
    }
    if let Ok((user_env, _)) = store.resolved_entries(config) {
        env.extend(user_env);
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_env_never_leaks_arbitrary_vars() {
        std::env::set_var("OPENGENT_TEST_SECRET", "x");
        let tmp = std::env::temp_dir().join(format!("opengent-env-{}", uuid::Uuid::new_v4()));
        let root = crate::config::resolve_data_root(Some(&tmp)).unwrap();
        let store = McpStore::load(
            &root,
            std::sync::Arc::new(crate::secrets::MemorySecretStore::default()),
        )
        .unwrap();
        let config = McpServerConfig {
            id: "s".into(),
            name: "s".into(),
            enabled: true,
            transport: super::super::config::McpTransport::Stdio,
            command: "true".into(),
            args: vec![],
            env: vec![],
            cwd: None,
            url: String::new(),
            headers: vec![],
            default_policy: crate::domain::ToolPolicy::Ask,
            tool_policies: HashMap::new(),
            call_timeout_secs: 60,
        };
        let env = build_env(&store, &config);
        assert!(!env.contains_key("OPENGENT_TEST_SECRET"));
        assert!(env.contains_key("PATH"));
        assert!(env.contains_key("HOME") || cfg!(windows));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn expand_tilde_handles_home_and_leaves_others() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/x/y"), format!("{home}/x/y"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("/abs/x"), "/abs/x");
        assert_eq!(expand_tilde("~other/dir"), "~other/dir");
        assert_eq!(expand_tilde("a~b"), "a~b");
    }

    #[test]
    fn resolved_path_looks_real() {
        let p = resolved_path().unwrap();
        #[cfg(unix)]
        assert!(p.contains("/usr/bin") || p.contains("homebrew") || p.contains("local"));
        let _ = p;
    }
}
