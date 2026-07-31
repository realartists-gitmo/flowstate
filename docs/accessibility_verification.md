# Verifying accessibility on a real screen reader

`./heaven.sh a11y` proves the accessibility tree is **constructed** correctly —
roles, names, document text, caret position, heading levels, table cells. It
does that by reading `Window::debug_a11y_tree_json()` inside `#[gpui::test]`.

It does **not** prove the tree is **delivered** to a real assistive-technology
client, nor that the result is pleasant to listen to. Those need a desktop
session and a human. This file is the procedure.

## What is already known about the delivery path

The AT-SPI stack on a Linux dev box is usually already there — `at-spi2-core`
provides the accessibility bus, and you can confirm it answers:

```sh
dbus-send --session --dest=org.a11y.Bus --print-reply /org/a11y/bus \
  org.a11y.Bus.GetAddress
# -> string "unix:path=/run/user/1000/at-spi/bus_N"
```

Applications currently exposing themselves can be listed with:

```sh
A11Y=$(dbus-send --session --dest=org.a11y.Bus --print-reply /org/a11y/bus \
  org.a11y.Bus.GetAddress | grep -o 'unix:path=[^"]*')
busctl --address="$A11Y" call org.a11y.atspi.Registry \
  /org/a11y/atspi/accessible/root org.a11y.atspi.Accessible GetChildren
```

**Flowstate does not appear in that list when run headlessly under `xvfb-run`.**
Two explanations were not distinguished, and whoever does this pass should find
out which it is:

1. `accesskit_unix` registers/activates lazily and needs a live AT client
   (no screen reader was running) — the production analogue of the
   `TestWindow::a11y_init` gap that `vendor/gpui` patches for tests; or
2. the window never came up properly under Xvfb (the probe logs
   `MESA: info: vulkan: No DRI3 support detected - required for presentation`),
   so there was nothing to expose.

If it turns out to be (1), that is worth knowing: it means the tree only exists
when someone is listening, which is by design but makes casual verification
impossible without a screen reader attached.

## The pass itself

Run on a real desktop session, not over SSH/Xvfb.

1. Install and start a screen reader — on Linux, `orca` (also needs
   `speech-dispatcher` for speech; braille optional).
2. Launch flowstate normally: `cargo run --release -p flowstate`.
3. Confirm it appears as an accessible application using the `busctl` command
   above. If it does not, that is finding (1) or (2) and blocks the rest.

Then check, in order — each of these has a corresponding automated test, so a
discrepancy means the automated test is asserting the wrong thing:

- **The document reads.** Arrow through it. Every paragraph should be spoken;
  headings should be announced as headings with a level.
- **The caret tracks.** Typing should echo. Moving by word/line should announce
  the right unit — this exercises `character_lengths` / `word_starts`, which is
  where an off-by-one shows up as garbled review.
- **Styled spans are distinguishable.** A citation, a highlight and struck text
  should each sound different from body text. Struck text should say
  "struck through".
- **Tables navigate.** Table navigation keys should move by cell and announce
  row/column; a header row should be repeated.
- **The outline and flow board navigate as trees**, announcing level and
  expanded state; a flow box should name its column (speech).
- **Dialogs trap focus.** Opening comments/share/settings should move focus into
  the dialog, Tab should stay inside it, and dismissing should return focus.
- **Nothing is announced as a bare "button".** This is the one thing the
  automated suite guarantees, so a violation means a control is bypassing
  `gpui_component::Button`.

## Known limits to judge against

- Only paragraphs the virtual list has materialized are in the tree. Reading
  past the viewport relies on `ScrollIntoView`; if continuous reading stalls at
  the viewport edge, that handler is the place to look.
- Equations are announced as their LaTeX source.
- Images with no alt text are announced as "image" with no description. That is
  deliberate — inventing a description would be worse — but it is worth noting
  how often it happens on real documents.
