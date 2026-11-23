//! First-party argument parsing: `--flag`, `--flag value`, `--flag=value`, `-h`/`-V`, and
//! `--` to end options (everything after it is positional, e.g. a prompt starting with `-`).

use std::collections::VecDeque;

use crate::CliError;

#[derive(Debug, PartialEq)]
pub enum Arg {
    /// Flag name without dashes, and a value given inline as `--name=value`.
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
            s if s.len() > 1 && s.starts_with('-') => Arg::Flag(s[1..].to_owned(), None),
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
    CliError::Usage(format!("unknown option `--{flag}`"))
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
    fn missing_value_is_a_usage_error() {
        let mut a = args(&["--model"]);
        a.next_arg();
        assert!(matches!(a.value("model", None), Err(CliError::Usage(_))));
        assert!(switch("json", Some("x".into())).is_err());
    }
}
