# Organization
For novel features, prefer setting them into new files. Place in the correct crates.
Avoid files over 1000 LOC. When found, suggest modularizing.

# UI development
Always prefer using the gpui-component library components over GPUI primitives, unless it cannot be made fit.

# Crate usage
Always consider pre-existing crates to handle operations, especially computationally heavy tasks (searching, replacement, or anything that has likely been solved externally already). Use cargo via the CLI rather than directly editing cargo files.

# Post-edit checks
Usually avoid `cargo check`, `cargo build`, `cargo run`, or `cargo fmt`.
Main agents should run `cargo clippy` when intended edits are implemented. Fix clippy suggestions if applicable (if it is not false positive or causing regression).

# Pull request requirements
If asked to push changes or manage a pull request, ensure the following is passed (unless a tool is not installed):
- `cargo clippy` - if issues shouldn't be fixed, apply relevant exceptions
- `cargo machete`
- `cargo fmt`
- `cargo build`
- `cargo deny`
- `cargo audit`
