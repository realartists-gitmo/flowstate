# Flowstate
The performant, narrow text editor for debaters.

# Building
To build Flowstate, you need to have [Cargo](https://doc.rust-lang.org/cargo/) installed.
Run `cargo run --package flowstate --release` to build and run.

# Logging
Flowstate writes a daily-rotated log file (`flowstate.log`) to its data directory.
By default it logs **only `error`-level events** so it stays quiet; raise the
verbosity with the environment variables below.

| Variable | Values | Effect |
|---|---|---|
| `FLOWSTATE_LOG_LEVEL` | `error` \| `warn` \| `info` \| `debug` \| `trace` | Sets the level for Flowstate's own crates (`flowstate`, `flowstate_collab`, `gpui_flowtext`); dependencies stay at `error`. Simplest knob. |
| `FLOWSTATE_LOG` | an [`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) directive | Full control, including dependency crates, e.g. `flowstate_collab=trace,flowstate::collab=debug`. Takes precedence over `RUST_LOG` and `FLOWSTATE_LOG_LEVEL`. |
| `RUST_LOG` | an `EnvFilter` directive | Same as `FLOWSTATE_LOG` but the standard variable; used if `FLOWSTATE_LOG` is unset. |
| `FLOWSTATE_LOG_STDOUT` | `1` \| `true` \| `yes` \| `on` | Also mirror logs to stdout (in addition to the file). |
| `FLOWSTATE_LOG_DIR` | a directory path | Override where the log file is written. |

Precedence for the level/filter: `FLOWSTATE_LOG` > `RUST_LOG` > `FLOWSTATE_LOG_LEVEL` > default (`error`).

Examples:
```sh
# Verbose Flowstate logs, mirrored to the terminal:
FLOWSTATE_LOG_LEVEL=debug FLOWSTATE_LOG_STDOUT=1 cargo run --package flowstate

# Fine-grained control over specific targets:
FLOWSTATE_LOG="flowstate_collab=trace,flowstate::collab=debug" cargo run --package flowstate
```

# License
Flowstate is licensed under the GNU Affero General Public License v3.0.
