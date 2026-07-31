# Verifying accessibility on a real screen reader

`./heaven.sh a11y` proves the accessibility tree is **constructed** correctly —
roles, names, document text, caret position, heading levels, table cells. It
does that by reading `Window::debug_a11y_tree_json()` inside `#[gpui::test]`.

Delivery to a real assistive-technology client HAS been verified once, over
AT-SPI, and the recipe is below. What still needs a human is whether the result
is *pleasant and coherent to listen to* — reading order, verbosity, phrasing.

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

**RESOLVED: accessibility is lazily activated, and nothing is exposed until a
screen reader is enabled.** `accesskit_unix` watches
`org.a11y.Status.ScreenReaderEnabled` (`context.rs:153-179`,
`StatusProxy::receive_screen_reader_enabled_changed`) and only then registers
and builds a tree. With it off, flowstate is absent from the AT-SPI registry
even though the window is up — verified separately with `xdotool` that the
process is alive and windows exist. This is the production analogue of the
`TestWindow::a11y_init` gap that `vendor/gpui` patches for tests.

Note the flag is NOT writable over D-Bus — `Properties.Set` on
`org.a11y.Status` silently fails. The real lever is gsettings, which is what
Orca toggles:

```sh
gsettings set org.gnome.desktop.a11y.applications screen-reader-enabled true
# ... run flowstate ...
gsettings set org.gnome.desktop.a11y.applications screen-reader-enabled false
```

With that flipped, flowstate DOES register: the AT-SPI registry listed
`screenshot_probe` alongside the desktop's own apps, and its `Name` reads back
correctly over the bus.

**But the CONTENTS were not reachable, and this is the open question.** A
recursive walker (correctly pairing each child's `(bus_name, path)` tuple —
`scratchpad/atspi_walk2.py`, worth rewriting rather than hunting for) reached
only two nodes: the application object and one unnamed child. `GetRoleName`
failed on both, while the same call against another running app
(`psst-gui`) returned `"application"` fine — so the transport is right and the
problem is on our side.

The leading theory, unproven: **gpui only ships a `TreeUpdate` on a drawn
frame**, and only when a11y was active at BOTH the start and end of that frame
(`window.rs:2942-2965`). `screenshot_probe` renders a static document and then
idles, so if activation lands after its last frame, no tree is ever published —
the adapter registers, but there is nothing in it. A real app that keeps
drawing (caret blink, input) would not have this problem, which is why the pass
below should use `cargo run -p flowstate`, NOT the probe.

An attempt to confirm via gpui's "Accessibility activated" `log::info!` was
inconclusive: `screenshot_probe` installs no logger, so that output goes nowhere
regardless. Adding a logger to the probe, or using the real app, would settle
it.

So: registration is verified, contents are NOT. Resolve that first — it is
likely a test-harness artifact rather than a product bug, but it has not been
shown to be.

## The pass itself

Run on a real desktop session, not over SSH/Xvfb.

1. Install and start a screen reader — on Linux, `orca` (also needs
   `speech-dispatcher` for speech; braille optional).
2. Launch flowstate normally: `cargo run --release -p flowstate`.
3. Confirm it appears as an accessible application using the `busctl` command
   above. Orca sets `screen-reader-enabled` itself, so no manual gsettings flip
   is needed when it is running. If flowstate is still absent, that is a
   regression in the delivery path, not a known limitation.

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
