//! Turning provider configuration into an executable command.

use crate::config::Provider;

/// The placeholder a provider's `args` use to receive the job's prompt.
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";

/// A command as the OS takes it: a program and an argument vector.
///
/// Deliberately not a shell string. A prompt contains quotes, newlines and
/// `$`; substituting it into a shell string would be an injection bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

#[must_use]
pub fn render_command(provider: &Provider, prompt: &str) -> CommandSpec {
    CommandSpec {
        program: provider.cmd.clone(),
        args: provider
            .args
            .iter()
            .map(|arg| arg.replace(PROMPT_PLACEHOLDER, prompt))
            .collect(),
    }
}
