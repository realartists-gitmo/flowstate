# All-capabilities extension

This trusted example exercises every Flowstate extension host import plus WASI
filesystem, networking, clocks, randomness, stdout, and stderr. Its destructive
buttons are labeled explicitly. Read the source before approving it.

The WIT file is copied from Flowstate's ABI so this standalone crate can build
outside the application workspace. Keep it synchronized with
`crates/flowstate-extension/wit/extension.wit`.

## Build and install

```sh
rustup target add wasm32-wasip2
cargo build --manifest-path examples/extensions/all-capabilities/Cargo.toml \
  --target wasm32-wasip2 --release

dest="${XDG_DATA_HOME:-$HOME/.local/share}/flowstate/extensions/com.flowstate.example.all-capabilities"
install -Dm644 examples/extensions/all-capabilities/extension.toml "$dest/extension.toml"
install -Dm644 \
  examples/extensions/all-capabilities/target/wasm32-wasip2/release/flowstate_all_capabilities_example.wasm \
  "$dest/extension.wasm"
```

Open Flowstate's Extensions side-panel section, reload extensions, and run a
button. Directory grants become available at the returned mount path on the
next invocation. The filesystem action records its observations in
`/data/capabilities.log`; `/extension` is read-only and `/document` is available
only for document actions.

## Buttons

- **Inspect document and selection** reads both snapshot APIs, reports them in
  the panel, and changes its own label for the current session.
- **Replace selection** and **Delete selection** demonstrate normalized,
  generation-checked rich-text edits.
- **Insert styled blocks** appends a styled paragraph, equation, table, and SVG
  image asset in one undoable batch.
- **Replace selected table cell** exercises nested table editing.
- **Refresh from disk** exercises reload and Flowstate's dirty-document prompt.
- **Exercise scoped filesystems** reads `/extension`, writes `/data`, and probes
  the saved document directory.
- **Fetch example.com** performs an outbound HTTP request over WASI sockets.
- **Request directory access** records a deferred read/write grant; **Write to
  last directory grant** exercises it on a later invocation.
- **Exercise clock, random, and output** uses WASI time and randomness and emits
  captured stdout and stderr.
- **Run until cancelled** intentionally loops so the Cancel button and Wasmtime
  epoch interruption can be tested.

After approving a directory, run **Write to last directory grant** in a later
invocation. The request action saves the returned mount path in `/data`, and the
follow-up action writes `flowstate-extension-example.txt` through that preopen.

**Run until cancelled** intentionally spins forever to demonstrate Flowstate's
Cancel control and Wasmtime epoch interruption. The selection, table-cell,
refresh, and block insertion actions mutate the open document.

The network action sends a plain HTTP request to `example.com:80` to demonstrate
WASI sockets without requiring TLS support in this small example.
