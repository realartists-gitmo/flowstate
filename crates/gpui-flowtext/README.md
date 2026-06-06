# gpui-flowtext

`gpui-flowtext` is a GPUI rich text editor component and document engine for applications that need high-performance, heavily customized document editing.

It provides the editor, document model, virtualization, layout, rendering, selection, editing operations, persistence, and host integration hooks. It does not ship an application-specific style system. Host applications define their own paragraph styles, inline semantic styles, highlight styles, labels, shortcuts, file extensions, and export behavior.

This repository contains the library implementation. Full API guides and examples will live on the external documentation site.

## Status

This project is early and actively being extracted from a production GPUI application. Public API names are being shaped around real integration use cases and may still change before a stable release.

## What It Includes

- GPUI rich text editor element and editor state
- Virtualized document rendering for large documents
- Paragraph, inline, highlight, table, image, and equation document primitives
- Selection, clipboard, history, formatting, and edit operation support
- Native binary document persistence
- Host hooks for styles, assets, export, events, and document-specific behavior

## What It Does Not Include

- A default app style set
- Product-specific paragraph names such as headings, tags, citations, or analysis styles
- DOCX/PDF exporters by default
- A prescribed file extension for host applications

The default native extension is `gptx`, but applications can use their own extension when exporting or saving native documents.

## API Tiers

The crate exposes grouped modules so applications can depend on the smallest practical surface:

- `gpui_flowtext::prelude` for common editor and document types
- `gpui_flowtext::style` for style slots, style catalogs, and theme types
- `gpui_flowtext::editor_api` for editor configuration, commands, events, and state
- `gpui_flowtext::persistence_api` for native document read/write helpers
- `gpui_flowtext::host` for host adapters such as export and asset integration
- `gpui_flowtext::advanced` for lower-level collaboration and edit operation APIs

The root module also re-exports the full API for convenience.

## Styling Model

`gpui-flowtext` uses generic style slots:

- `ParagraphStyle::Normal`
- `ParagraphStyle::Custom(slot)`
- `RunSemanticStyle::Plain`
- `RunSemanticStyle::Custom(slot)`
- `HighlightStyle::Custom(slot)`

Applications own the meaning of those slots. A host app can map slot `0` to a heading, slot `1` to a warning, slot `2` to a transcript speaker, or anything else. The library only stores, lays out, renders, and edits the styles according to the `DocumentTheme` and style catalog the host provides.

## Persistence

Native documents can be read and written with:

```rust
use gpui_flowtext::persistence_api::{read_document, write_document};
```

The built-in native extension is:

```rust
use gpui_flowtext::persistence_api::DEFAULT_DOCUMENT_EXTENSION;
```

Host applications can use their own extension with `DocumentExportFormat::NativeWithExtension(...)`.

## Development

Run the library tests:

```sh
cargo test
```

Run a type check:

```sh
cargo check
```

Optional instrumentation features are available for hotpath profiling:

```sh
cargo check --features hotpath
```

## License

See [LICENSE](LICENSE).
