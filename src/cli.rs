//! The command line: which flags exist, and what to do with the ones that do not.
//!
//! Two rules, and the second one is the reason this is a module with tests instead of a few lines at
//! the top of `main.rs`:
//!
//! 1. the usage text lists **only** flags that exist. A help line advertising `--json` when there is
//!    no JSON is a small lie, and small lies in a tool whose whole job is to be trusted are not small;
//! 2. a flag nobody knows is an **error**, not silence. `--fail-on-share` (a typo) used to be accepted
//!    and ignored, which is indistinguishable from a gate that passed: exactly the failure mode this
//!    project exists to catch in migrations.

use std::collections::BTreeMap;

/// Flags, as they were given on the command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// The command, first word.
    pub command: String,
    /// Values per flag, in the order they appeared.
    pub flags: BTreeMap<String, Vec<String>>,
}

impl Args {
    /// Parses `--flag value`, `--flag=value` and bare words.
    ///
    /// It does not know which flags are valid: [`Args::validate`] does, because that depends on the
    /// command.
    pub fn parse(raw: &[String]) -> Result<Self, String> {
        let mut args = Self::default();
        let mut rest = raw.iter();
        args.command = rest.next().cloned().unwrap_or_default();
        while let Some(token) = rest.next() {
            let Some(key) = token.strip_prefix("--") else {
                return Err(format!("unexpected argument {token:?}"));
            };
            if let Some((key, value)) = key.split_once('=') {
                args.flags
                    .entry(key.to_owned())
                    .or_default()
                    .push(value.to_owned());
                continue;
            }
            let value = rest
                .next()
                .ok_or_else(|| format!("--{key} needs a value"))?;
            args.flags
                .entry(key.to_owned())
                .or_default()
                .push(value.clone());
        }
        Ok(args)
    }

    /// Refuses flags that the command does not have.
    pub fn validate(&self) -> Result<(), String> {
        let allowed = allowed_flags(&self.command);
        for key in self.flags.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!(
                    "unknown flag --{key} for `{}`: it has {}. A flag that is ignored is a gate that silently does nothing",
                    self.command,
                    if allowed.is_empty() {
                        "no flags".to_owned()
                    } else {
                        allowed
                            .iter()
                            .map(|flag| format!("--{flag}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
            }
        }
        Ok(())
    }

    /// The last value given for a flag.
    pub fn one(&self, key: &str) -> Option<&str> {
        self.flags
            .get(key)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    /// Every value given for a flag, in order.
    pub fn many(&self, key: &str) -> &[String] {
        self.flags.get(key).map_or(&[], Vec::as_slice)
    }

    /// The last value given for a flag, or an error naming the flag.
    pub fn required(&self, key: &str) -> Result<&str, String> {
        self.one(key).ok_or_else(|| format!("--{key} is required"))
    }
}

/// The flags each command accepts. Exhaustive on purpose: a new flag has to be added here, or it does
/// not work, which is the opposite of the silence it would otherwise get.
#[must_use]
pub fn allowed_flags(command: &str) -> &'static [&'static str] {
    match command {
        "plan" => &["migration", "fail-on"],
        "prove" => &["db-url", "migration"],
        "audit" => &["db-url"],
        "report" => &["db-url", "migration", "safe", "seed", "out"],
        // `help`, and anything that is not a command: no flags, and the caller says why
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Args {
        Args::parse(
            &raw.iter()
                .map(|token| (*token).to_owned())
                .collect::<Vec<_>>(),
        )
        .expect("parses")
    }

    #[test]
    fn both_spellings_of_a_flag_work() {
        assert_eq!(
            args(&["plan", "--migration", "a.sql"]).one("migration"),
            Some("a.sql")
        );
        assert_eq!(
            args(&["plan", "--migration=a.sql"]).one("migration"),
            Some("a.sql")
        );
    }

    #[test]
    fn the_last_value_wins_but_all_of_them_are_kept() {
        let parsed = args(&["report", "--statement", "a=x.sql", "--statement", "b=y.sql"]);
        assert_eq!(parsed.many("statement").len(), 2);
        assert_eq!(parsed.one("statement"), Some("b=y.sql"));
    }

    #[test]
    fn a_flag_without_a_value_is_an_error() {
        let error =
            Args::parse(&["plan".to_owned(), "--migration".to_owned()]).expect_err("no value");
        assert!(error.contains("needs a value"), "{error}");
    }

    #[test]
    fn a_positional_argument_that_is_not_a_flag_is_an_error() {
        let error = Args::parse(&["plan".to_owned(), "a.sql".to_owned()]).expect_err("positional");
        assert!(error.contains("unexpected argument"), "{error}");
    }

    #[test]
    fn an_unknown_flag_is_refused_instead_of_being_ignored() {
        // the reason this module exists: `--fail-on-share` (a typo) used to be accepted and dropped,
        // which looks exactly like a passing gate
        let parsed = args(&["plan", "--migration", "a.sql", "--fail-on-share", "share"]);
        let error = parsed.validate().expect_err("unknown flag");
        assert!(error.contains("unknown flag --fail-on-share"), "{error}");
        assert!(error.contains("--fail-on"), "{error}");
    }

    #[test]
    fn the_flags_a_command_has_are_accepted() {
        args(&["plan", "--migration", "a.sql", "--fail-on", "share"])
            .validate()
            .expect("known");
        args(&[
            "report",
            "--db-url",
            "u",
            "--migration",
            "a.sql",
            "--out",
            "o.md",
        ])
        .validate()
        .expect("known");
        // an entirely different command: its flags are not this command's
        let error = args(&["audit", "--migration", "a.sql"])
            .validate()
            .expect_err("--migration is not audit's");
        assert!(error.contains("unknown flag --migration"), "{error}");
    }

    #[test]
    fn every_flag_the_usage_text_lists_exists() {
        // the help text is a promise: this test is what keeps `--json` out of it until there is JSON
        let usage = include_str!("../src/main.rs");
        for command in ["plan", "prove", "audit", "report"] {
            let line = usage
                .lines()
                .find(|line| line.trim_start().starts_with(command))
                .unwrap_or_else(|| panic!("{command} is not in the usage text"));
            for token in line
                .split_whitespace()
                .filter(|token| token.starts_with("--"))
            {
                let flag = token.trim_start_matches("--");
                assert!(
                    allowed_flags(command).contains(&flag),
                    "the usage text promises --{flag} for {command}, which does not accept it"
                );
            }
        }
    }
}
