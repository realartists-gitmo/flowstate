# Verifying accessibility on a real screen reader

`./heaven.sh a11y` proves the accessibility tree is **constructed** correctly —
roles, names, document text, caret position, heading levels, table cells. It
does that by reading `Window::debug_a11y_tree_json()` inside `#[gpui::test]`.

Delivery to a real assistive-technology client IS verified — both reading the
tree and driving the UI through it. Details below. What still needs a human is
whether the result is *pleasant and coherent to listen to*: reading order,
verbosity, phrasing.

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

**CONTENTS ARE CONFIRMED**, against the real binary. `tools/atspi_walk.py`
walks the tree over the bus; the result on a fresh window is:

```
application "flowstate"
  frame "Flowstate"
    push button "Flowstate" / "File" / "Insert" / "Document" / "Collaborate" /
                "View" / "Share" / "Settings"
    tool bar "Ribbon"
      push button "Undo (Ctrl+Z)" / "Pocket (F4)" / "Cite (F8)" / ... (28)
    "Document outline"
    tab list -> tab "*Untitled1.db8" -> push button "Close document"
    "Document"            <- Role::MultilineTextInput + aria_label
      paragraph           <- the document's paragraph node
    status bar "Status"
```

Every name there is one we set: the tooltip fallback on icon buttons, the
landmark labels, the editor's `aria_label`.

**The action path is confirmed too.** Querying the "New Doc" button over AT-SPI
returns `a(sss) 1 "click" "" ""`, and `DoAction(0)` returns `true` — after which
the tree changes, gaining the ribbon, tab bar and document. So an assistive
technology client can both read the UI and drive it.

Two gotchas for anyone writing another walker:

* **accesskit does not implement `GetRoleName`** over AT-SPI, only the numeric
  `GetRole`; nor `org.a11y.atspi.Text` on these nodes. A walker that relies on
  `GetRoleName` sees `?` everywhere and concludes, wrongly, that nothing is
  exposed.
* **`GetChildren` returns `a(so)`** — (bus_name, path) pairs. Each child must be
  addressed with ITS OWN bus name; reusing the parent's stops the walk at the
  first level.

Note the tree only publishes on a DRAWN FRAME with a11y active at both its start
and end (`window.rs:2942-2965`). A static window that stops drawing (like
`screenshot_probe`) registers but never publishes — use the real binary.

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
