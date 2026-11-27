//! Pure logic shared by the worker and the CLI. No I/O here, so it stays testable on the host.

/// Engine version, taken from the workspace so it never drifts.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_semver_like() {
        assert_eq!(VERSION.split('.').count(), 3);
    }
}
