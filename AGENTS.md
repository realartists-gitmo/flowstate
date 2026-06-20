# Organization
For novel features, prefer setting them into new files. Place in the correct crates.

# UI development
Always prefer using the gpui-component library components over GPUI primitives, unless it cannot be made fit.

# Crate usage
Always consider pre-existing crates to handle operations, especially computationally heavy tasks (searching, replacement, or anything that has likely been solved externally already). Use cargo via the CLI rather than directly editing cargo files.

# Post-edit checks
Avoid `cargo check`, `cargo build`, `cargo run`, or `cargo fmt`.
Main agents should run `cargo clippy` when ALL intended edits are implemented. Fix clippy suggestions if applicable (if it is not false positive or causing regression).
Never EVER run any verification command like this until you can call your goal fully complete. Do not run it after 'stages,' or 'individual changes.' Do not run it when you are 'not finished' with the goal but still ending your turn.
