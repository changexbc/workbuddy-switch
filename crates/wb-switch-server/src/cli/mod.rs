//! CLI shell around shared core services.
mod accounts;
mod args;
mod control;
mod credits;
mod execute;
mod output;
mod protocol;
mod summary;
mod target;
#[cfg(test)]
mod tests;
pub use args::{parse, Command, Options, HELP};
pub use execute::execute;
pub use output::{print_failure, print_result};
pub use protocol::Context;
