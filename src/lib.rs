//! colony 2 library crate.

pub mod backend;
pub mod cli;
pub mod commands;
pub mod ctx;
pub mod discover;
pub mod envelope;
pub mod state;
pub mod target;
pub mod telegram;
pub mod time;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert_eq!(crate::VERSION, "1.2.0");
    }
}
