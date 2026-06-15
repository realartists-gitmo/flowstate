//! Environment-variable parsing for runtime options. Lives in its own
//! integration-test binary with a single test: the env must be set before
//! any option is first read (each option caches its value on first `get`),
//! and `set_var` must not race other test threads.

use rimalloc::options::{self, Option as Opt};

#[test]
fn env_values_parse_with_fallback() {
    // SAFETY: single test in this binary; no other thread reads the
    // environment concurrently.
    unsafe {
        std::env::set_var("MIMALLOC_VERBOSE", "on");
        std::env::set_var("MIMALLOC_SHOW_STATS", "NO");
        std::env::set_var("MIMALLOC_SHOW_ERRORS", "3");
        std::env::set_var("MIMALLOC_PURGE_EXTEND_DELAY", "");
        std::env::set_var("MIMALLOC_EAGER_COMMIT_DELAY", "garbage");
    }
    assert_eq!(options::get(Opt::Verbose), 1); // "on" => 1
    assert_eq!(options::get(Opt::ShowStats), 0); // "NO" => 0 (case-insensitive)
    assert_eq!(options::get(Opt::ShowErrors), 3); // numeric
    assert_eq!(options::get(Opt::PurgeExtendDelay), 1); // "" => 1
    assert_eq!(options::get(Opt::EagerCommitDelay), 0); // unparsable => default
    // unset env: default applies
    assert_eq!(options::get(Opt::FullPageRetain), 2);
}
