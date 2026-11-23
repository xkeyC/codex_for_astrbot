//! fork: `PolicyParser` without the Starlark interpreter (feature `starlark`
//! off). Empty rule files parse to an empty policy; anything else is an error.

use crate::error::Error;
use crate::error::Result;
use crate::policy::Policy;

#[derive(Default)]
pub struct PolicyParser {
    _private: (),
}

impl PolicyParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse(&mut self, policy_identifier: &str, policy_file_contents: &str) -> Result<()> {
        let has_rules = policy_file_contents
            .lines()
            .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'));
        if has_rules {
            return Err(Error::Unsupported(policy_identifier.to_string()));
        }
        Ok(())
    }

    pub fn build(self) -> Policy {
        Policy::empty()
    }
}
