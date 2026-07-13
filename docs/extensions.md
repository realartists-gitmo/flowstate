# Flowstate extensions

Flowstate extensions are trusted WebAssembly components executed by Wasmtime.
An extension may be written in any language that can produce a WebAssembly
Component Model component, including a component that bundles its own Python or
JavaScript interpreter.

Flowstate does not compile Python or JavaScript for extension authors. The
installed artifact must already implement the `flowstate:extension@1.0.0` WIT
world defined in `crates/flowstate-extension/wit/extension.wit`.

## Installation

Install each extension in its own directory:

```text
~/.local/share/flowstate/extensions/com.example.rewriter/
  extension.toml
  extension.wasm
```

On non-Linux platforms the base directory follows the operating system's normal
user data directory convention.

An extension manifest has this shape:

```toml
manifest_version = 1
id = "com.example.rewriter"
name = "Rewriter"
version = "1.0.0"
component = "extension.wasm"

[[actions]]
id = "rewrite-selection"
label = "Rewrite selection"
requires_document = true

[[actions]]
id = "open-dashboard"
label = "Open dashboard"
requires_document = false
```

The component path is relative to the extension directory and cannot escape it.
Extension and action IDs must be dot-separated identifiers containing ASCII
letters, numbers, `-`, or `_`.

## UI and activation

Flowstate shows one Extensions icon in the existing side-panel rail. The panel
contains one collapsible section per installed extension and renders every
manifest action as a button. Pressing a button calls the component's
`run(action-id)` export.

Flowstate asks for confirmation the first time an extension runs. Approval is
bound to the component digest, so replacing the component prompts again.
Extensions are arbitrary trusted code within the WASI capabilities described
below; only install components you trust.

An extension can update its button labels for the current Flowstate session by
calling `set-action-label`. Labels return to their manifest values after a
restart or extension reload.

## Runtime capabilities

Every action gets a fresh component instance. Flowstate caches compiled
components, permits one running action per extension, and allows actions from
different extensions to run concurrently. The panel exposes cancellation and
captures bounded stdout and stderr output.

The guest receives these pre-opened directories:

- `/extension`: the installation directory, read-only.
- `/data`: persistent extension-specific data, read/write.
- `/document`: the active saved document's directory, read/write, only when a
  document-required action is invoked.

The runtime provides WASI clocks, randomness, HTTP, and sockets. It does not
inherit Flowstate's environment or provide native process spawning.

An extension can call `request-directory-access` when it needs another folder.
Flowstate shows the requested read-only or read/write mode and lets the user
choose the directory. Approved grants are tied to the component digest and are
mounted on later invocations under the returned `/grants/<grant-id>` path. A
changed component cannot reuse an earlier grant without asking again.

## Document API

The imported host interface provides JSON snapshots and generation-checked edit
batches. A snapshot contains the current caret or selection, selected text, rich
document blocks, and an edit generation. Submit that generation with edits;
Flowstate rejects stale offsets if the document changed in the meantime.

Accepted edits are validated as a complete batch and committed as one undoable
editor operation. They pass through the normal editor pipeline so layout,
autosave, recovery, outline, and collaboration state remain consistent.

`refresh-from-disk` reloads the active native document. If the document has
unsaved changes, Flowstate asks the user before discarding them. Untitled and
imported documents cannot be refreshed from disk.
