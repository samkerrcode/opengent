//! First-party argument parsing: `--flag`, `--flag value`, `--flag=value`, `-h`/`-V`, and
//! `--` to end options (everything after it is positional, e.g. a prompt starting with `-`).

use std::collections::VecDeque;

use crate::CliError;

#[derive(Debug, PartialEq)]
pub enum Arg {
    /// Flag name without its `--`, and a value given inline as `--name=value`. A short
    /// option other than `-h`/`-V` keeps its dash (`-x`): none exist, so it is only ever
    /// reported back as typed (`unknown`).
    Flag(String, Option<String>),
    Positional(String),
}

pub struct Args {
    items: VecDeque<String>,
    positional_only: bool,
}

impl Args {
    pub fn new(items: impl IntoIterator<Item = String>) -> Self {
        Self {
            items: items.into_iter().collect(),
            positional_only: false,
        }
    }

    /// The next argument without treating it as a flag (subcommand names).
    pub fn word(&mut self) -> Option<String> {
        self.items.pop_front()
    }

    /// Whether `--help` or `-h` is among the remaining options (before any `--`).
    pub fn wants_help(&self) -> bool {
        self.items
            .iter()
            .take_while(|a| *a != "--")
            .any(|a| a == "--help" || a == "-h")
    }

    pub fn next_arg(&mut self) -> Option<Arg> {
        let item = self.items.pop_front()?;
        if self.positional_only {
            return Some(Arg::Positional(item));
        }
        if item == "--" {
            self.positional_only = true;
            return self.next_arg();
        }
        if let Some(flag) = item.strip_prefix("--") {
            return Some(match flag.split_once('=') {
                Some((name, value)) => Arg::Flag(name.to_owned(), Some(value.to_owned())),
                None => Arg::Flag(flag.to_owned(), None),
            });
        }
        Some(match item.as_str() {
            "-h" => Arg::Flag("help".into(), None),
            "-V" => Arg::Flag("version".into(), None),
            s if s.len() > 1 && s.starts_with('-') => Arg::Flag(s.to_owned(), None),
            _ => Arg::Positional(item),
        })
    }

    /// The value of `--flag`: inline (`--flag=value`) or the next argument.
    pub fn value(&mut self, flag: &str, inline: Option<String>) -> Result<String, CliError> {
        if let Some(v) = inline {
            return Ok(v);
        }
        match self.items.pop_front() {
            Some(v) if v != "--" => Ok(v),
            _ => Err(CliError::Usage(format!("--{flag} needs a value"))),
        }
    }
}

/// Rejects `--flag=value` for a switch.
pub fn switch(flag: &str, inline: Option<String>) -> Result<(), CliError> {
    match inline {
        None => Ok(()),
        Some(_) => Err(CliError::Usage(format!("--{flag} takes no value"))),
    }
}

pub fn unknown(flag: &str) -> CliError {
    let dashes = if flag.starts_with('-') { "" } else { "--" };
    CliError::Usage(format!("unknown option `{dashes}{flag}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Args {
        Args::new(items.iter().map(|s| s.to_string()))
    }

    #[test]
    fn flags_values_and_positionals() {
        let mut a = args(&["--json", "--model=p/m", "--mcp", "fs", "hi", "-h", "-"]);
        assert_eq!(a.next_arg(), Some(Arg::Flag("json".into(), None)));
        assert_eq!(
            a.next_arg(),
            Some(Arg::Flag("model".into(), Some("p/m".into())))
        );
        assert_eq!(a.next_arg(), Some(Arg::Flag("mcp".into(), None)));
        assert_eq!(a.value("mcp", None).unwrap(), "fs");
        assert_eq!(a.next_arg(), Some(Arg::Positional("hi".into())));
        assert_eq!(a.next_arg(), Some(Arg::Flag("help".into(), None)));
        assert_eq!(a.next_arg(), Some(Arg::Positional("-".into())));
        assert_eq!(a.next_arg(), None);
    }

    #[test]
    fn double_dash_ends_options() {
        let mut a = args(&["--", "--not-a-flag", "-x"]);
        assert_eq!(a.next_arg(), Some(Arg::Positional("--not-a-flag".into())));
        assert_eq!(a.next_arg(), Some(Arg::Positional("-x".into())));
    }

    #[test]
    fn help_is_found_among_options_only() {
        assert!(args(&["--agent", "a", "--help"]).wants_help());
        assert!(args(&["-h"]).wants_help());
        assert!(!args(&["--", "--help"]).wants_help());
        assert!(!args(&["what does --helpful mean"]).wants_help());
    }

    #[test]
    fn unknown_options_are_reported_as_typed() {
        let message = |a: Option<Arg>| match a {
            Some(Arg::Flag(f, _)) => match unknown(&f) {
                CliError::Usage(m) => m,
                e => panic!("{e:?}"),
            },
            a => panic!("{a:?}"),
        };
        let mut a = args(&["-x", "--bogus"]);
        assert_eq!(message(a.next_arg()), "unknown option `-x`");
        assert_eq!(message(a.next_arg()), "unknown option `--bogus`");
    }

    #[test]
    fn missing_value_is_a_usage_error() {
        let mut a = args(&["--model"]);
        a.next_arg();
        assert!(matches!(a.value("model", None), Err(CliError::Usage(_))));
        assert!(switch("json", Some("x".into())).is_err());
    }
}
