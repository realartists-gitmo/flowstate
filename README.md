# Flowstate
The performant, multiplayer word processor for debaters.

# Installation
Flowstate is in early alpha. To build Flowstate, you need to have [Cargo](https://doc.rust-lang.org/cargo/) installed. You also need to enable nightly rust; we use it for compiler optimizations. You can do this by running `rustup override set nightly`.
Run `cargo run --package flowstate --release` to build and run.

# Roadmap
- [x] Blazing-fast text editor with all Verbatim styles and Anthony Trufanov's Undertag and Analytic.
- [x] Full support for importing and exporting .docx Verbatim documents with instant stylepox cleaning. Pending full support for .cmir.
- [x] Native .db8 file format that halves the size of .docx for large documents, with significantly faster read and write.
- [x] Tab-based editor with freeform multipanelling, a built-in filesystem tree, and easy navigation.
- [ ] Native structural and spreadsheet-style flow support via native .fl0 file format, with freeform marker annotations that work seamlessly during collaboration.
- [x] Full functionality on Windows, Linux, and Mac.
- [ ] Full feature parity with Verbatim.
- [ ] Full feature parity with CardMirror.
- [ ] Web version with Google Drive and Dropbox integration.
- [ ] Mobile.
- [x] Drag-and-drop lightning-fast index of your entire tub with rendered previews, searchable by blocks, tags, cites, or anything else.
- [ ] Block aliases like '/condogood' that resolve instantly, without you ever having to open up another document or menu.
- [ ] Live, synchronous, peer-to-peer collaboration with the whole squad. No external server, eternally free and safe.
- [x] Instant invisiblity mode, instant ctrl-F, instant find-and-replace--even on impact defense.
- [x] Infinite themeability, with a huge built-in roster from which to choose.
- [ ] Maximal extensibility--write any functionality you want in Rust, Python, Javascript, Typescript, C, C++, or C#.
- [ ] Infinite style freedom, with no risk of stylepox or inaccessibility. Everybody's editor starts with a small, universal set of styles and purely client-side appearance. Need more styles? Enable them for yourself, and everything you send to others automatically converts to an analogous and recognizable style if they haven't enabled yours.
- [x] Compression algorithim specially-trained on representative strata from OpenEvidence to achieve transporation speed jumps on debate documents as high as 3x.

# License
Flowstate is licensed under the GNU Affero General Public License v3.0.
