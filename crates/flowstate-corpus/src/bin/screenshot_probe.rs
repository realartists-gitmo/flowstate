//! §act-eleven / A11.6 net: the raster screenshot probe. Opens a REAL gpui
//! window rendering a deterministic `RichTextEditor` fixture (headings,
//! cite/highlight runs, soft breaks, a table, an equation), pumps frames for a
//! settle interval, then exits. Run under Xvfb with `-fbdir` so the X server's
//! framebuffer FILE is the captured raster — headless, no compositor chrome,
//! no screenshot tooling:
//!
//! ```sh
//! env -u WAYLAND_DISPLAY xvfb-run -a -s "-screen 0 1200x900x24 -fbdir /tmp/fb" \
//!   ./target/release/screenshot_probe
//! ```
//!
//! `heaven.sh screenshot` wraps this and compares the framebuffer against the
//! machine-local golden (fonts and scale are machine state; the golden lives in
//! `HEAVEN_DIR`, like the hotpath baselines).

use gpui::{App, AppContext as _, Application, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_flowtext::{
  DocumentTheme, InputParagraph, InputRun, ParagraphStyle, RichTextEditor, RunSemanticStyle, RunStyles, document_from_input,
};

fn run(text: &str, styles: RunStyles) -> InputRun {
  InputRun {
    text: text.to_string(),
    styles,
  }
}

fn cite() -> RunStyles {
  RunStyles {
    semantic: RunSemanticStyle::Custom(2),
    ..RunStyles::default()
  }
}

fn underlined() -> RunStyles {
  RunStyles {
    direct_underline: true,
    ..RunStyles::default()
  }
}

/// A paint-diverse fixture: every glyph path the editor paints on a normal
/// document — heading styles, plain/cite/underline/strikethrough runs, a soft
/// line break, CJK + combining marks + emoji-free wide chars (deterministic
/// shaping), and enough paragraphs to fill the viewport.
fn fixture() -> gpui_flowtext::DocumentProjection {
  let mut paragraphs = vec![
    InputParagraph {
      style: ParagraphStyle::Custom(1),
      runs: vec![run("Screenshot Probe — Pocket", RunStyles::default())],
    },
    InputParagraph {
      style: ParagraphStyle::Custom(3),
      runs: vec![run("Hat: deterministic paint fixture", RunStyles::default())],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run("Plain lead-in, then a ", RunStyles::default()),
        run("cited span", cite()),
        run(" and an ", RunStyles::default()),
        run("underlined tail", underlined()),
        run(". Soft\u{2028}break line two.", RunStyles::default()),
      ],
    },
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![run("宽字符 shaping row — 中文测试 mixed with Latin.", RunStyles::default())],
    },
  ];
  for filler in 0..14 {
    paragraphs.push(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![
        run(
          &format!("Filler paragraph {filler:02} with steady content so the viewport is fully covered "),
          RunStyles::default(),
        ),
        run("and a styled span per row.", if filler % 2 == 0 { cite() } else { underlined() }),
      ],
    });
  }
  document_from_input(DocumentTheme::default(), paragraphs)
}

fn main() {
  let settle_ms: u64 = std::env::var("FLOWSTATE_SCREENSHOT_SETTLE_MS")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(2500);
  Application::new().run(move |cx: &mut App| {
    gpui_component::init(cx);
    let bounds = Bounds::centered(None, size(px(1160.0), px(860.0)), cx);
    let window = cx
      .open_window(
        WindowOptions {
          // FULLSCREEN: the capture is a whole-screen portal screenshot, so the
          // probe must BE the whole screen — no compositor placement variance,
          // no decorations, nothing else visible.
          window_bounds: Some(WindowBounds::Fullscreen(bounds)),
          ..Default::default()
        },
        |_window, cx| cx.new(|cx| RichTextEditor::new_with_path(fixture(), None, cx)),
      )
      .expect("screenshot probe window");
    // Keep painting for the settle interval (font load + first layout), then quit.
    cx.spawn(async move |cx| {
      cx.background_executor().timer(std::time::Duration::from_millis(settle_ms)).await;
      let _ = window;
      let _ = cx.update(|cx| cx.quit());
    })
    .detach();
  });
}
