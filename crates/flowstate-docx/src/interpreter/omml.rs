//! Minimal Office `MathML` (OMML, `m:oMath`) → `LaTeX` conversion for DOCX import.
//!
//! Word stores equations as OMML, while the Flowstate equation model stores a
//! single LaTeX source string ([`InputEquationSyntax::Latex`] is the only syntax
//! the equation block exposes). No available crate converts OMML → LaTeX (the
//! `mitex` crate converts LaTeX → Typst), so this module implements a focused,
//! best-effort converter over the common OMML constructs (runs/text, fractions,
//! sub/superscripts, radicals, delimiters, n-ary operators, functions, accents,
//! overbars, limits and matrices). Unrecognized constructs degrade to their
//! concatenated literal text so that symbols are preserved rather than dropped.

use flowstate_document::{InputEquationBlock, InputEquationDisplay, InputEquationSyntax};
use quick_xml::{
  Reader as XmlReader,
  events::{BytesStart, Event},
};

/// Parse a captured OMML container (`m:oMath`, `m:oMathPara`, or any element that
/// wraps them) and return one [`InputEquationBlock`] per `m:oMath`, in document
/// order. `m:oMath` inside an `m:oMathPara` becomes a display equation; a bare
/// `m:oMath` becomes an inline-like equation.
#[hotpath::measure]
pub(super) fn equations_from_container_bytes(bytes: &[u8]) -> Vec<InputEquationBlock> {
  let Some(root) = parse_tree(bytes) else {
    return Vec::new();
  };
  let mut equations = Vec::new();
  collect_equations(&root, false, &mut equations);
  equations
}

/// Returns `true` when the captured bytes look like they contain Office Math, so
/// callers can cheaply skip non-math captured XML (bookmarks, SDTs, ...).
pub(super) fn contains_office_math(bytes: &[u8]) -> bool {
  memmem(bytes, b"oMath")
}

fn collect_equations(node: &Node, in_math_para: bool, out: &mut Vec<InputEquationBlock>) {
  if node.local == "oMath" {
    let source = latex_from_children(node).trim().to_string();
    if !source.is_empty() {
      out.push(InputEquationBlock {
        source,
        syntax: InputEquationSyntax::Latex,
        display: if in_math_para {
          InputEquationDisplay::Display
        } else {
          InputEquationDisplay::InlineLikeParagraph
        },
      });
    }
    return;
  }
  let nested = in_math_para || node.local == "oMathPara";
  for child in &node.children {
    collect_equations(child, nested, out);
  }
}

struct Node {
  local: String,
  val: Option<String>,
  text: String,
  children: Vec<Node>,
}

fn parse_tree(bytes: &[u8]) -> Option<Node> {
  let mut reader = XmlReader::from_reader(bytes);
  reader.config_mut().trim_text(false);
  let mut buf = Vec::new();
  let mut stack: Vec<Node> = Vec::new();
  let mut root: Option<Node> = None;

  loop {
    match reader.read_event_into(&mut buf) {
      Ok(Event::Start(event)) => stack.push(node_from_start(&event)),
      Ok(Event::Empty(event)) => {
        let node = node_from_start(&event);
        if let Some(parent) = stack.last_mut() {
          parent.children.push(node);
        } else {
          root.get_or_insert(node);
        }
      },
      Ok(Event::Text(event)) => {
        if let Some(top) = stack.last_mut()
          && let Ok(text) = event.xml10_content()
        {
          top.text.push_str(&text);
        }
      },
      Ok(Event::End(_)) => {
        if let Some(node) = stack.pop() {
          if let Some(parent) = stack.last_mut() {
            parent.children.push(node);
          } else {
            root.get_or_insert(node);
          }
        }
      },
      Ok(Event::Eof) => break,
      Err(_) => return None,
      _ => {},
    }
    buf.clear();
  }

  root
}

fn node_from_start(event: &BytesStart<'_>) -> Node {
  let local = local_name(event.name().as_ref()).to_owned();
  let mut val = None;
  for attr in event.attributes().flatten() {
    if local_name(attr.key.as_ref()) == "val" {
      val = std::str::from_utf8(attr.value.as_ref())
        .ok()
        .map(|value| value.to_owned());
    }
  }
  Node {
    local,
    val,
    text: String::new(),
    children: Vec::new(),
  }
}

fn local_name(name: &[u8]) -> &str {
  let name = std::str::from_utf8(name).unwrap_or_default();
  name.rsplit(':').next().unwrap_or(name)
}

fn child<'node>(node: &'node Node, local: &str) -> Option<&'node Node> {
  node.children.iter().find(|child| child.local == local)
}

fn latex_from_children(node: &Node) -> String {
  let mut out = String::new();
  for child in &node.children {
    out.push_str(&latex_from_node(child));
  }
  out
}

fn latex_from_arg(node: &Node, local: &str) -> String {
  child(node, local)
    .map(latex_from_children)
    .unwrap_or_default()
}

fn latex_from_node(node: &Node) -> String {
  match node.local.as_str() {
    "t" => map_text(&node.text),
    "r" => latex_from_children(node),
    "f" => format!("\\frac{{{}}}{{{}}}", latex_from_arg(node, "num"), latex_from_arg(node, "den")),
    "sSup" => format!("{}^{{{}}}", braced(&latex_from_arg(node, "e")), latex_from_arg(node, "sup")),
    "sSub" => format!("{}_{{{}}}", braced(&latex_from_arg(node, "e")), latex_from_arg(node, "sub")),
    "sSubSup" | "sPre" => format!(
      "{}_{{{}}}^{{{}}}",
      braced(&latex_from_arg(node, "e")),
      latex_from_arg(node, "sub"),
      latex_from_arg(node, "sup")
    ),
    "rad" => {
      let inner = latex_from_arg(node, "e");
      let degree = latex_from_arg(node, "deg");
      if degree.is_empty() {
        format!("\\sqrt{{{inner}}}")
      } else {
        format!("\\sqrt[{degree}]{{{inner}}}")
      }
    },
    "d" => {
      let (open, close) = delimiter_chars(node);
      let inner = node
        .children
        .iter()
        .filter(|child| child.local == "e")
        .map(latex_from_children)
        .collect::<Vec<_>>()
        .join(",");
      format!("\\left{open}{inner}\\right{close}")
    },
    "nary" => latex_nary(node),
    "func" => {
      let name = latex_from_arg(node, "fName");
      let argument = latex_from_arg(node, "e");
      if argument.is_empty() { name } else { format!("{name} {argument}") }
    },
    "acc" => latex_accent(node),
    "bar" => format!("\\overline{{{}}}", latex_from_arg(node, "e")),
    "limLow" => format!("{}_{{{}}}", braced(&latex_from_arg(node, "e")), latex_from_arg(node, "lim")),
    "limUpp" => format!("{}^{{{}}}", braced(&latex_from_arg(node, "e")), latex_from_arg(node, "lim")),
    "m" => latex_matrix(node),
    "box" | "borderBox" | "groupChr" | "eqArr" | "e" | "num" | "den" | "oMath" => latex_from_children(node),
    // Property containers carry presentation hints only, never rendered content.
    name if name.ends_with("Pr") => String::new(),
    _ => latex_from_children(node),
  }
}

fn braced(value: &str) -> String {
  if value.chars().count() == 1 {
    value.to_owned()
  } else {
    format!("{{{value}}}")
  }
}

fn delimiter_chars(node: &Node) -> (String, String) {
  let properties = child(node, "dPr");
  let beginning = properties
    .and_then(|properties| child(properties, "begChr"))
    .and_then(|chr| chr.val.clone());
  let ending = properties
    .and_then(|properties| child(properties, "endChr"))
    .and_then(|chr| chr.val.clone());
  (latex_delimiter(beginning.as_deref(), "("), latex_delimiter(ending.as_deref(), ")"))
}

fn latex_delimiter(chr: Option<&str>, default: &str) -> String {
  match chr {
    None => default.to_owned(),
    Some("") => ".".to_owned(),
    Some("{") => "\\{".to_owned(),
    Some("}") => "\\}".to_owned(),
    Some("‖") => "\\|".to_owned(),
    Some("⟨") => "\\langle".to_owned(),
    Some("⟩") => "\\rangle".to_owned(),
    Some("⌊") => "\\lfloor".to_owned(),
    Some("⌋") => "\\rfloor".to_owned(),
    Some("⌈") => "\\lceil".to_owned(),
    Some("⌉") => "\\rceil".to_owned(),
    Some(other) => other.to_owned(),
  }
}

fn latex_nary(node: &Node) -> String {
  let chr = child(node, "naryPr")
    .and_then(|properties| child(properties, "chr"))
    .and_then(|chr| chr.val.clone());
  let mut out = nary_operator(chr.as_deref());
  let sub = latex_from_arg(node, "sub");
  if !sub.is_empty() {
    out.push_str("_{");
    out.push_str(&sub);
    out.push('}');
  }
  let sup = latex_from_arg(node, "sup");
  if !sup.is_empty() {
    out.push_str("^{");
    out.push_str(&sup);
    out.push('}');
  }
  let operand = latex_from_arg(node, "e");
  if !operand.is_empty() {
    out.push(' ');
    out.push_str(&operand);
  }
  out
}

fn nary_operator(chr: Option<&str>) -> String {
  match chr {
    None | Some("∑") => "\\sum".to_owned(),
    Some("∏") => "\\prod".to_owned(),
    Some("∐") => "\\coprod".to_owned(),
    Some("∫") => "\\int".to_owned(),
    Some("∬") => "\\iint".to_owned(),
    Some("∭") => "\\iiint".to_owned(),
    Some("∮") => "\\oint".to_owned(),
    Some("⋃") => "\\bigcup".to_owned(),
    Some("⋂") => "\\bigcap".to_owned(),
    Some("⋁") => "\\bigvee".to_owned(),
    Some("⋀") => "\\bigwedge".to_owned(),
    Some(other) => other.to_owned(),
  }
}

fn latex_accent(node: &Node) -> String {
  let chr = child(node, "accPr")
    .and_then(|properties| child(properties, "chr"))
    .and_then(|chr| chr.val.clone());
  let command = match chr.as_deref() {
    Some("\u{0303}" | "~") => "\\tilde",
    Some("\u{0304}" | "¯") => "\\bar",
    Some("\u{0307}") => "\\dot",
    Some("\u{0308}") => "\\ddot",
    Some("\u{20D7}" | "→") => "\\vec",
    Some("\u{0306}") => "\\breve",
    Some("\u{030C}") => "\\check",
    Some("\u{0301}") => "\\acute",
    Some("\u{0300}") => "\\grave",
    _ => "\\hat",
  };
  format!("{command}{{{}}}", latex_from_arg(node, "e"))
}

fn latex_matrix(node: &Node) -> String {
  let rows = node
    .children
    .iter()
    .filter(|child| child.local == "mr")
    .map(|row| {
      row
        .children
        .iter()
        .filter(|child| child.local == "e")
        .map(latex_from_children)
        .collect::<Vec<_>>()
        .join(" & ")
    })
    .collect::<Vec<_>>()
    .join(" \\\\ ");
  format!("\\begin{{matrix}}{rows}\\end{{matrix}}")
}

fn map_text(text: &str) -> String {
  let mut out = String::new();
  for ch in text.chars() {
    if let Some(mapped) = map_symbol(ch) {
      out.push_str(mapped);
    } else {
      push_escaped(&mut out, ch);
    }
  }
  out
}

fn push_escaped(out: &mut String, ch: char) {
  match ch {
    '%' | '$' | '#' | '&' | '_' | '{' | '}' => {
      out.push('\\');
      out.push(ch);
    },
    '\\' => out.push_str("\\backslash "),
    _ => out.push(ch),
  }
}

#[allow(clippy::too_many_lines, reason = "A single flat symbol table is the clearest mapping form.")]
fn map_symbol(ch: char) -> Option<&'static str> {
  Some(match ch {
    'α' => "\\alpha ",
    'β' => "\\beta ",
    'γ' => "\\gamma ",
    'δ' => "\\delta ",
    'ε' => "\\epsilon ",
    'ζ' => "\\zeta ",
    'η' => "\\eta ",
    'θ' => "\\theta ",
    'ι' => "\\iota ",
    'κ' => "\\kappa ",
    'λ' => "\\lambda ",
    'μ' => "\\mu ",
    'ν' => "\\nu ",
    'ξ' => "\\xi ",
    'π' => "\\pi ",
    'ρ' => "\\rho ",
    'σ' => "\\sigma ",
    'τ' => "\\tau ",
    'υ' => "\\upsilon ",
    'φ' => "\\phi ",
    'χ' => "\\chi ",
    'ψ' => "\\psi ",
    'ω' => "\\omega ",
    'Γ' => "\\Gamma ",
    'Δ' => "\\Delta ",
    'Θ' => "\\Theta ",
    'Λ' => "\\Lambda ",
    'Ξ' => "\\Xi ",
    'Π' => "\\Pi ",
    'Σ' => "\\Sigma ",
    'Φ' => "\\Phi ",
    'Ψ' => "\\Psi ",
    'Ω' => "\\Omega ",
    '×' => "\\times ",
    '÷' => "\\div ",
    '±' => "\\pm ",
    '∓' => "\\mp ",
    '⋅' => "\\cdot ",
    '∗' => "\\ast ",
    '≤' => "\\le ",
    '≥' => "\\ge ",
    '≠' => "\\ne ",
    '≈' => "\\approx ",
    '≡' => "\\equiv ",
    '∝' => "\\propto ",
    '→' => "\\to ",
    '←' => "\\leftarrow ",
    '⇒' => "\\Rightarrow ",
    '⇔' => "\\Leftrightarrow ",
    '↔' => "\\leftrightarrow ",
    '∞' => "\\infty ",
    '∂' => "\\partial ",
    '∇' => "\\nabla ",
    '∈' => "\\in ",
    '∉' => "\\notin ",
    '⊂' => "\\subset ",
    '⊆' => "\\subseteq ",
    '⊃' => "\\supset ",
    '⊇' => "\\supseteq ",
    '∪' => "\\cup ",
    '∩' => "\\cap ",
    '∀' => "\\forall ",
    '∃' => "\\exists ",
    '∅' => "\\emptyset ",
    '∧' => "\\wedge ",
    '∨' => "\\vee ",
    '¬' => "\\neg ",
    '∑' => "\\sum ",
    '∏' => "\\prod ",
    '∫' => "\\int ",
    '√' => "\\surd ",
    '°' => "^{\\circ}",
    '′' => "'",
    '″' => "''",
    _ => return None,
  })
}

fn memmem(haystack: &[u8], needle: &[u8]) -> bool {
  // §perf: memchr's SIMD substring search is O(n) vs the O(n·m) `windows` scan.
  // The empty-needle guard preserves the prior `false` result (memmem::find
  // returns Some(0) for an empty needle).
  !needle.is_empty() && memchr::memmem::find(haystack, needle).is_some()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn sources(bytes: &str) -> Vec<String> {
    equations_from_container_bytes(bytes.as_bytes())
      .into_iter()
      .map(|equation| equation.source)
      .collect()
  }

  #[test]
  fn inline_fraction_converts_to_latex() {
    let omml = r"<m:oMath><m:f><m:num><m:r><m:t>1</m:t></m:r></m:num><m:den><m:r><m:t>2</m:t></m:r></m:den></m:f></m:oMath>";
    let equations = equations_from_container_bytes(omml.as_bytes());
    assert_eq!(equations.len(), 1);
    assert_eq!(equations[0].source, "\\frac{1}{2}");
    assert_eq!(equations[0].display, InputEquationDisplay::InlineLikeParagraph);
  }

  #[test]
  fn math_para_marks_display_and_handles_superscript() {
    let omml =
      r"<m:oMathPara><m:oMath><m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath></m:oMathPara>";
    let equations = equations_from_container_bytes(omml.as_bytes());
    assert_eq!(equations.len(), 1);
    assert_eq!(equations[0].source, "x^{2}");
    assert_eq!(equations[0].display, InputEquationDisplay::Display);
  }

  #[test]
  fn nary_sum_uses_limits_and_operand() {
    let omml = r#"<m:oMath><m:nary><m:naryPr><m:chr m:val="∑"/></m:naryPr><m:sub><m:r><m:t>i=1</m:t></m:r></m:sub><m:sup><m:r><m:t>n</m:t></m:r></m:sup><m:e><m:r><m:t>i</m:t></m:r></m:e></m:nary></m:oMath>"#;
    assert_eq!(sources(omml), vec!["\\sum_{i=1}^{n} i".to_string()]);
  }

  #[test]
  fn radical_and_symbols_are_mapped() {
    let omml = r"<m:oMath><m:rad><m:deg/><m:e><m:r><m:t>α≤β</m:t></m:r></m:e></m:rad></m:oMath>";
    assert_eq!(sources(omml), vec!["\\sqrt{\\alpha \\le \\beta }".to_string()]);
  }

  #[test]
  fn non_math_xml_yields_no_equations() {
    let bookmark = r#"<w:bookmarkStart w:id="1" w:name="foo"/>"#;
    assert!(equations_from_container_bytes(bookmark.as_bytes()).is_empty());
    assert!(!contains_office_math(bookmark.as_bytes()));
  }
}
