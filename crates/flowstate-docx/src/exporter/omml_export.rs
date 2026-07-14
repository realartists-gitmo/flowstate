//! Focused LaTeX -> Office Math (OMML) conversion for DOCX equation export
//! (FS-125).
//!
//! [`crate::interpreter::omml`] converts OMML -> LaTeX on import; this module is
//! its export mirror. It covers the same constructs (runs/text, fractions,
//! sub/superscripts, radicals, delimiters, n-ary operators, functions, accents,
//! overbars, limits and matrices) plus a Greek/operator symbol table. Anything
//! the parser cannot make sense of (empty source, unbalanced braces, a missing
//! `\right`/`\end`) yields `None`, and the caller keeps the bracketed
//! `[Equation: source]` text fallback.
//!
//! The produced string is raw OMML injected verbatim into `word/document.xml`
//! by [`crate::exporter::xml_postprocess`]; that pass also declares `xmlns:m` on
//! `w:document`. `Display` equations are wrapped in `m:oMathPara`; inline-like
//! equations produce a bare `m:oMath`.

/// Convert LaTeX `source` to an OMML fragment. `display` selects
/// `m:oMathPara`-wrapped output. Returns `None` for unconvertible source.
pub(super) fn latex_to_omml(source: &str, display: bool) -> Option<String> {
  let trimmed = source.trim();
  if trimmed.is_empty() {
    return None;
  }
  let tokens = tokenize(trimmed);
  let mut parser = Parser {
    tokens,
    pos: 0,
    failed: false,
  };
  let nodes = parser.parse_sequence(false);
  if parser.failed {
    return None;
  }
  let body = render_nodes(&nodes);
  if body.trim().is_empty() {
    return None;
  }
  let omath = format!("<m:oMath>{body}</m:oMath>");
  if display {
    Some(format!("<m:oMathPara>{omath}</m:oMathPara>"))
  } else {
    Some(omath)
  }
}

// -- Tokenizer ---------------------------------------------------------------

#[derive(Clone)]
enum Token {
  /// `\word` (letters) or `\<single-non-letter>` (e.g. `\{`, `\,`).
  Command(String),
  LBrace,
  RBrace,
  Sup,
  Sub,
  Amp,
  /// `\\` row separator (matrices).
  RowBreak,
  Char(char),
}

fn tokenize(source: &str) -> Vec<Token> {
  let mut tokens = Vec::new();
  let mut chars = source.chars().peekable();
  while let Some(c) = chars.next() {
    match c {
      '\\' => match chars.peek().copied() {
        Some('\\') => {
          chars.next();
          tokens.push(Token::RowBreak);
        },
        Some(next) if next.is_ascii_alphabetic() => {
          let mut word = String::new();
          while let Some(&peeked) = chars.peek() {
            if peeked.is_ascii_alphabetic() {
              word.push(peeked);
              chars.next();
            } else {
              break;
            }
          }
          tokens.push(Token::Command(word));
        },
        Some(next) => {
          chars.next();
          tokens.push(Token::Command(next.to_string()));
        },
        None => {},
      },
      '{' => tokens.push(Token::LBrace),
      '}' => tokens.push(Token::RBrace),
      '^' => tokens.push(Token::Sup),
      '_' => tokens.push(Token::Sub),
      '&' => tokens.push(Token::Amp),
      c if c.is_whitespace() => {},
      c => tokens.push(Token::Char(c)),
    }
  }
  tokens
}

// -- AST ---------------------------------------------------------------------

enum Node {
  /// Literal math text (already symbol-mapped; XML-escaped at render time).
  Text(String),
  /// A run rendered on its own (function names); never merged with neighbours.
  Ident(String),
  Group(Vec<Node>),
  Sub(Box<Node>, Vec<Node>),
  Sup(Box<Node>, Vec<Node>),
  SubSup(Box<Node>, Vec<Node>, Vec<Node>),
  Frac(Vec<Node>, Vec<Node>),
  /// `Sqrt(optional degree, radicand)`.
  Sqrt(Option<Vec<Node>>, Vec<Node>),
  /// `Delim(open, close, inner)` with resolved delimiter characters.
  Delim(String, String, Vec<Node>),
  /// `Nary(operator, sub, sup, operand)`.
  Nary(String, Vec<Node>, Vec<Node>, Vec<Node>),
  /// `Accent(combining char, base)`.
  Accent(String, Vec<Node>),
  Bar(Vec<Node>),
  /// `LimLow(base, lower limit)`.
  LimLow(Vec<Node>, Vec<Node>),
  /// `Matrix(optional delimiters, rows of cells)`.
  Matrix(Option<(String, String)>, Vec<Vec<Vec<Node>>>),
}

// -- Parser ------------------------------------------------------------------

struct Parser {
  tokens: Vec<Token>,
  pos: usize,
  failed: bool,
}

impl Parser {
  fn peek(&self) -> Option<Token> {
    self.tokens.get(self.pos).cloned()
  }

  fn bump(&mut self) {
    self.pos += 1;
  }

  /// Parse a run of atoms until end-of-input or a closing brace. When
  /// `stop_at_rbrace` is set the terminating `}` is consumed; reaching the end
  /// without it (or an unmatched `}` at top level) marks the parse failed.
  fn parse_sequence(&mut self, stop_at_rbrace: bool) -> Vec<Node> {
    let mut nodes = Vec::new();
    loop {
      match self.peek() {
        None => {
          if stop_at_rbrace {
            self.failed = true;
          }
          break;
        },
        Some(Token::RBrace) => {
          self.bump();
          if !stop_at_rbrace {
            self.failed = true;
          }
          break;
        },
        _ => {
          let Some(atom) = self.parse_atom() else {
            break;
          };
          let node = self.apply_scripts(atom);
          nodes.push(node);
        },
      }
    }
    nodes
  }

  /// Parse a single atom without trailing scripts. Returns `None` at a stopping
  /// token (`}`) or end-of-input.
  fn parse_atom(&mut self) -> Option<Node> {
    match self.peek()? {
      Token::RBrace => None,
      Token::LBrace => {
        self.bump();
        Some(Node::Group(self.parse_sequence(true)))
      },
      Token::Char(c) => {
        self.bump();
        Some(Node::Text(c.to_string()))
      },
      // Stray separators / scripts without a base degrade to nothing.
      Token::Amp | Token::RowBreak | Token::Sup | Token::Sub => {
        self.bump();
        Some(Node::Text(String::new()))
      },
      Token::Command(name) => {
        self.bump();
        Some(self.parse_command(&name))
      },
    }
  }

  fn apply_scripts(&mut self, base: Node) -> Node {
    let mut sub: Option<Vec<Node>> = None;
    let mut sup: Option<Vec<Node>> = None;
    loop {
      match self.peek() {
        Some(Token::Sub) if sub.is_none() => {
          self.bump();
          sub = Some(self.parse_arg());
        },
        Some(Token::Sup) if sup.is_none() => {
          self.bump();
          sup = Some(self.parse_arg());
        },
        _ => break,
      }
    }
    match (sub, sup) {
      (None, None) => base,
      (Some(sub), None) => Node::Sub(Box::new(base), sub),
      (None, Some(sup)) => Node::Sup(Box::new(base), sup),
      (Some(sub), Some(sup)) => Node::SubSup(Box::new(base), sub, sup),
    }
  }

  /// Parse one command argument: a braced group or a single atom.
  fn parse_arg(&mut self) -> Vec<Node> {
    match self.peek() {
      Some(Token::LBrace) => {
        self.bump();
        self.parse_sequence(true)
      },
      Some(_) => self.parse_atom().map(|node| vec![node]).unwrap_or_default(),
      None => Vec::new(),
    }
  }

  fn parse_command(&mut self, name: &str) -> Node {
    match name {
      "frac" | "dfrac" | "tfrac" | "cfrac" => {
        let num = self.parse_arg();
        let den = self.parse_arg();
        Node::Frac(num, den)
      },
      "sqrt" => {
        let degree = self.parse_optional_bracket();
        let radicand = self.parse_arg();
        Node::Sqrt(degree, radicand)
      },
      "overline" => Node::Bar(self.parse_arg()),
      "left" => {
        let open = self.parse_delimiter();
        let inner = self.parse_until_right();
        let close = self.parse_delimiter();
        Node::Delim(open, close, inner)
      },
      // A stray `\right` outside `\left` degrades to nothing.
      "right" => Node::Text(String::new()),
      "begin" => self.parse_environment(),
      "end" => {
        self.skip_group();
        Node::Text(String::new())
      },
      "lim" => {
        if matches!(self.peek(), Some(Token::Sub)) {
          self.bump();
          Node::LimLow(vec![Node::Ident("lim".to_string())], self.parse_arg())
        } else {
          Node::Ident("lim".to_string())
        }
      },
      "text" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "mathtt" | "mathbb" | "mathcal" | "mathfrak" | "boldsymbol" | "operatorname" => {
        Node::Group(self.parse_arg())
      },
      _ => {
        if let Some(chr) = accent_char(name) {
          return Node::Accent(chr.to_string(), self.parse_arg());
        }
        if let Some(op) = nary_operator(name) {
          let (sub, sup) = self.parse_nary_scripts();
          return Node::Nary(op.to_string(), sub, sup, Vec::new());
        }
        if is_function(name) {
          return Node::Ident(name.to_string());
        }
        if let Some(symbol) = command_symbol(name) {
          return Node::Text(symbol.to_string());
        }
        if let Some(literal) = literal_command(name) {
          return Node::Text(literal.to_string());
        }
        if is_spacing(name) {
          return Node::Text(" ".to_string());
        }
        // Unknown command: drop it (best-effort) rather than fail the whole
        // equation, so a single unsupported macro does not force text fallback.
        Node::Text(String::new())
      },
    }
  }

  /// Parse the `_..` / `^..` limits that immediately follow an n-ary operator.
  fn parse_nary_scripts(&mut self) -> (Vec<Node>, Vec<Node>) {
    let mut sub = Vec::new();
    let mut sup = Vec::new();
    loop {
      match self.peek() {
        Some(Token::Sub) if sub.is_empty() => {
          self.bump();
          sub = self.parse_arg();
        },
        Some(Token::Sup) if sup.is_empty() => {
          self.bump();
          sup = self.parse_arg();
        },
        _ => break,
      }
    }
    (sub, sup)
  }

  /// Parse an optional `[..]` degree group (for `\sqrt[n]{x}`).
  fn parse_optional_bracket(&mut self) -> Option<Vec<Node>> {
    if !matches!(self.peek(), Some(Token::Char('['))) {
      return None;
    }
    self.bump();
    let mut nodes = Vec::new();
    loop {
      match self.peek() {
        None => break,
        Some(Token::Char(']')) => {
          self.bump();
          break;
        },
        _ => {
          let Some(atom) = self.parse_atom() else {
            break;
          };
          nodes.push(self.apply_scripts(atom));
        },
      }
    }
    Some(nodes)
  }

  /// Read the delimiter following `\left` or `\right`.
  fn parse_delimiter(&mut self) -> String {
    match self.peek() {
      Some(Token::Char(c)) => {
        self.bump();
        delimiter_from_char(c)
      },
      Some(Token::Command(name)) => {
        self.bump();
        delimiter_from_command(&name)
      },
      _ => String::new(),
    }
  }

  fn parse_until_right(&mut self) -> Vec<Node> {
    let mut nodes = Vec::new();
    loop {
      match self.peek() {
        None => {
          self.failed = true;
          break;
        },
        Some(Token::Command(name)) if name == "right" => {
          self.bump();
          break;
        },
        Some(Token::RBrace) => {
          self.failed = true;
          self.bump();
          break;
        },
        _ => {
          let Some(atom) = self.parse_atom() else {
            break;
          };
          nodes.push(self.apply_scripts(atom));
        },
      }
    }
    nodes
  }

  fn parse_environment(&mut self) -> Node {
    let env = self.read_environment_name();
    let delimiters = match env.as_str() {
      "pmatrix" => Some(("(".to_string(), ")".to_string())),
      "bmatrix" => Some(("[".to_string(), "]".to_string())),
      "Bmatrix" => Some(("{".to_string(), "}".to_string())),
      "vmatrix" => Some(("|".to_string(), "|".to_string())),
      "Vmatrix" => Some(("\u{2016}".to_string(), "\u{2016}".to_string())),
      _ => None,
    };
    let rows = self.parse_matrix_body();
    Node::Matrix(delimiters, rows)
  }

  fn read_environment_name(&mut self) -> String {
    let mut name = String::new();
    if matches!(self.peek(), Some(Token::LBrace)) {
      self.bump();
    }
    loop {
      match self.peek() {
        Some(Token::RBrace) => {
          self.bump();
          break;
        },
        Some(Token::Char(c)) => {
          name.push(c);
          self.bump();
        },
        Some(Token::Command(part)) => {
          name.push_str(&part);
          self.bump();
        },
        _ => break,
      }
    }
    name
  }

  fn parse_matrix_body(&mut self) -> Vec<Vec<Vec<Node>>> {
    let mut rows: Vec<Vec<Vec<Node>>> = Vec::new();
    let mut row: Vec<Vec<Node>> = Vec::new();
    let mut cell: Vec<Node> = Vec::new();
    loop {
      match self.peek() {
        None => {
          self.failed = true;
          break;
        },
        Some(Token::Command(name)) if name == "end" => {
          self.bump();
          self.skip_group();
          break;
        },
        Some(Token::Amp) => {
          self.bump();
          row.push(std::mem::take(&mut cell));
        },
        Some(Token::RowBreak) => {
          self.bump();
          row.push(std::mem::take(&mut cell));
          rows.push(std::mem::take(&mut row));
        },
        _ => {
          let Some(atom) = self.parse_atom() else {
            break;
          };
          cell.push(self.apply_scripts(atom));
        },
      }
    }
    if !cell.is_empty() || !row.is_empty() {
      row.push(cell);
      rows.push(row);
    }
    rows
  }

  fn skip_group(&mut self) {
    if !matches!(self.peek(), Some(Token::LBrace)) {
      return;
    }
    self.bump();
    let mut depth = 1_usize;
    while depth > 0 {
      match self.peek() {
        None => break,
        Some(Token::LBrace) => {
          depth += 1;
          self.bump();
        },
        Some(Token::RBrace) => {
          depth -= 1;
          self.bump();
        },
        _ => self.bump(),
      }
    }
  }
}

// -- Renderer ----------------------------------------------------------------

fn render_nodes(nodes: &[Node]) -> String {
  let mut out = String::new();
  let mut text = String::new();
  for node in nodes {
    if let Node::Text(value) = node {
      text.push_str(value);
    } else {
      flush_text(&mut out, &mut text);
      out.push_str(&render_node(node));
    }
  }
  flush_text(&mut out, &mut text);
  out
}

fn flush_text(out: &mut String, text: &mut String) {
  if !text.is_empty() {
    out.push_str("<m:r><m:t>");
    out.push_str(&escape_text(text));
    out.push_str("</m:t></m:r>");
    text.clear();
  }
}

fn render_node(node: &Node) -> String {
  match node {
    Node::Text(value) => format!("<m:r><m:t>{}</m:t></m:r>", escape_text(value)),
    Node::Ident(value) => format!("<m:r><m:t>{}</m:t></m:r>", escape_text(value)),
    Node::Group(inner) => render_nodes(inner),
    Node::Sub(base, sub) => format!("<m:sSub><m:e>{}</m:e><m:sub>{}</m:sub></m:sSub>", render_node(base), render_nodes(sub)),
    Node::Sup(base, sup) => format!("<m:sSup><m:e>{}</m:e><m:sup>{}</m:sup></m:sSup>", render_node(base), render_nodes(sup)),
    Node::SubSup(base, sub, sup) => format!(
      "<m:sSubSup><m:e>{}</m:e><m:sub>{}</m:sub><m:sup>{}</m:sup></m:sSubSup>",
      render_node(base),
      render_nodes(sub),
      render_nodes(sup)
    ),
    Node::Frac(num, den) => format!("<m:f><m:num>{}</m:num><m:den>{}</m:den></m:f>", render_nodes(num), render_nodes(den)),
    Node::Sqrt(Some(degree), radicand) => format!(
      "<m:rad><m:deg>{}</m:deg><m:e>{}</m:e></m:rad>",
      render_nodes(degree),
      render_nodes(radicand)
    ),
    Node::Sqrt(None, radicand) => format!(
      "<m:rad><m:radPr><m:degHide m:val=\"1\"/></m:radPr><m:deg/><m:e>{}</m:e></m:rad>",
      render_nodes(radicand)
    ),
    Node::Delim(open, close, inner) => render_delimited(open, close, &render_nodes(inner)),
    Node::Nary(operator, sub, sup, operand) => render_nary(operator, sub, sup, operand),
    Node::Accent(chr, base) => format!(
      "<m:acc><m:accPr><m:chr m:val=\"{}\"/></m:accPr><m:e>{}</m:e></m:acc>",
      escape_attr(chr),
      render_nodes(base)
    ),
    Node::Bar(inner) => format!(
      "<m:bar><m:barPr><m:pos m:val=\"top\"/></m:barPr><m:e>{}</m:e></m:bar>",
      render_nodes(inner)
    ),
    Node::LimLow(base, limit) => format!(
      "<m:limLow><m:e>{}</m:e><m:lim>{}</m:lim></m:limLow>",
      render_nodes(base),
      render_nodes(limit)
    ),
    Node::Matrix(delimiters, rows) => {
      let mut matrix = String::from("<m:m>");
      for row in rows {
        matrix.push_str("<m:mr>");
        for cell in row {
          matrix.push_str("<m:e>");
          matrix.push_str(&render_nodes(cell));
          matrix.push_str("</m:e>");
        }
        matrix.push_str("</m:mr>");
      }
      matrix.push_str("</m:m>");
      match delimiters {
        Some((open, close)) => render_delimited(open, close, &matrix),
        None => matrix,
      }
    },
  }
}

fn render_delimited(open: &str, close: &str, inner: &str) -> String {
  format!(
    "<m:d><m:dPr><m:begChr m:val=\"{}\"/><m:endChr m:val=\"{}\"/></m:dPr><m:e>{}</m:e></m:d>",
    escape_attr(open),
    escape_attr(close),
    inner
  )
}

fn render_nary(operator: &str, sub: &[Node], sup: &[Node], operand: &[Node]) -> String {
  format!(
    "<m:nary><m:naryPr><m:chr m:val=\"{}\"/><m:limLoc m:val=\"undOvr\"/><m:subHide m:val=\"{}\"/><m:supHide m:val=\"{}\"/></m:naryPr><m:sub>{}</m:sub><m:sup>{}</m:sup><m:e>{}</m:e></m:nary>",
    escape_attr(operator),
    if sub.is_empty() { "1" } else { "0" },
    if sup.is_empty() { "1" } else { "0" },
    render_nodes(sub),
    render_nodes(sup),
    render_nodes(operand)
  )
}

// -- Symbol / delimiter tables -----------------------------------------------

fn escape_text(value: &str) -> String {
  value
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
  escape_text(value).replace('"', "&quot;")
}

fn is_spacing(name: &str) -> bool {
  matches!(
    name,
    "," | ";" | ":" | "!" | " " | "quad" | "qquad" | "thinspace" | "medspace" | "thickspace"
  )
}

fn literal_command(name: &str) -> Option<&'static str> {
  Some(match name {
    "{" => "{",
    "}" => "}",
    "%" => "%",
    "$" => "$",
    "#" => "#",
    "&" => "&",
    "_" => "_",
    "|" => "\u{2016}",
    "backslash" => "\\",
    _ => return None,
  })
}

fn is_function(name: &str) -> bool {
  matches!(
    name,
    "sin"
      | "cos"
      | "tan"
      | "cot"
      | "sec"
      | "csc"
      | "arcsin"
      | "arccos"
      | "arctan"
      | "sinh"
      | "cosh"
      | "tanh"
      | "coth"
      | "log"
      | "ln"
      | "lg"
      | "exp"
      | "det"
      | "dim"
      | "gcd"
      | "hom"
      | "ker"
      | "deg"
      | "arg"
      | "max"
      | "min"
      | "sup"
      | "inf"
      | "limsup"
      | "liminf"
      | "Pr"
  )
}

fn accent_char(name: &str) -> Option<&'static str> {
  Some(match name {
    "hat" | "widehat" => "\u{0302}",
    "tilde" | "widetilde" => "\u{0303}",
    "bar" => "\u{0304}",
    "vec" => "\u{20D7}",
    "dot" => "\u{0307}",
    "ddot" => "\u{0308}",
    "dddot" => "\u{20DB}",
    "check" => "\u{030C}",
    "breve" => "\u{0306}",
    "acute" => "\u{0301}",
    "grave" => "\u{0300}",
    "mathring" => "\u{030A}",
    _ => return None,
  })
}

fn nary_operator(name: &str) -> Option<&'static str> {
  Some(match name {
    "sum" => "\u{2211}",
    "prod" => "\u{220F}",
    "coprod" => "\u{2210}",
    "int" => "\u{222B}",
    "iint" => "\u{222C}",
    "iiint" => "\u{222D}",
    "oint" => "\u{222E}",
    "bigcup" => "\u{22C3}",
    "bigcap" => "\u{22C2}",
    "bigvee" => "\u{22C1}",
    "bigwedge" => "\u{22C0}",
    "bigoplus" => "\u{2A01}",
    "bigotimes" => "\u{2A02}",
    "bigodot" => "\u{2A00}",
    "biguplus" => "\u{2A04}",
    "bigsqcup" => "\u{2A06}",
    _ => return None,
  })
}

fn delimiter_from_char(c: char) -> String {
  match c {
    '(' => "(".to_string(),
    ')' => ")".to_string(),
    '[' => "[".to_string(),
    ']' => "]".to_string(),
    '|' => "|".to_string(),
    '/' => "/".to_string(),
    '.' => String::new(),
    '<' => "\u{27E8}".to_string(),
    '>' => "\u{27E9}".to_string(),
    other => other.to_string(),
  }
}

fn delimiter_from_command(name: &str) -> String {
  match name {
    "{" => "{".to_string(),
    "}" => "}".to_string(),
    "|" | "Vert" => "\u{2016}".to_string(),
    "vert" => "|".to_string(),
    "langle" => "\u{27E8}".to_string(),
    "rangle" => "\u{27E9}".to_string(),
    "lfloor" => "\u{230A}".to_string(),
    "rfloor" => "\u{230B}".to_string(),
    "lceil" => "\u{2308}".to_string(),
    "rceil" => "\u{2309}".to_string(),
    _ => String::new(),
  }
}

#[allow(clippy::too_many_lines, reason = "A single flat symbol table is the clearest mapping form.")]
fn command_symbol(name: &str) -> Option<&'static str> {
  Some(match name {
    "alpha" => "\u{03B1}",
    "beta" => "\u{03B2}",
    "gamma" => "\u{03B3}",
    "delta" => "\u{03B4}",
    "epsilon" | "varepsilon" => "\u{03B5}",
    "zeta" => "\u{03B6}",
    "eta" => "\u{03B7}",
    "theta" | "vartheta" => "\u{03B8}",
    "iota" => "\u{03B9}",
    "kappa" => "\u{03BA}",
    "lambda" => "\u{03BB}",
    "mu" => "\u{03BC}",
    "nu" => "\u{03BD}",
    "xi" => "\u{03BE}",
    "pi" => "\u{03C0}",
    "rho" => "\u{03C1}",
    "sigma" => "\u{03C3}",
    "tau" => "\u{03C4}",
    "upsilon" => "\u{03C5}",
    "phi" | "varphi" => "\u{03C6}",
    "chi" => "\u{03C7}",
    "psi" => "\u{03C8}",
    "omega" => "\u{03C9}",
    "Gamma" => "\u{0393}",
    "Delta" => "\u{0394}",
    "Theta" => "\u{0398}",
    "Lambda" => "\u{039B}",
    "Xi" => "\u{039E}",
    "Pi" => "\u{03A0}",
    "Sigma" => "\u{03A3}",
    "Phi" => "\u{03A6}",
    "Psi" => "\u{03A8}",
    "Omega" => "\u{03A9}",
    "times" => "\u{00D7}",
    "div" => "\u{00F7}",
    "pm" => "\u{00B1}",
    "mp" => "\u{2213}",
    "cdot" => "\u{22C5}",
    "ast" => "\u{2217}",
    "star" => "\u{22C6}",
    "circ" => "\u{2218}",
    "bullet" => "\u{2219}",
    "le" | "leq" => "\u{2264}",
    "ge" | "geq" => "\u{2265}",
    "ne" | "neq" => "\u{2260}",
    "approx" => "\u{2248}",
    "equiv" => "\u{2261}",
    "cong" => "\u{2245}",
    "sim" => "\u{223C}",
    "propto" => "\u{221D}",
    "ll" => "\u{226A}",
    "gg" => "\u{226B}",
    "to" | "rightarrow" => "\u{2192}",
    "leftarrow" => "\u{2190}",
    "leftrightarrow" => "\u{2194}",
    "Rightarrow" => "\u{21D2}",
    "Leftarrow" => "\u{21D0}",
    "Leftrightarrow" => "\u{21D4}",
    "mapsto" => "\u{21A6}",
    "infty" => "\u{221E}",
    "partial" => "\u{2202}",
    "nabla" => "\u{2207}",
    "in" => "\u{2208}",
    "notin" => "\u{2209}",
    "ni" => "\u{220B}",
    "subset" => "\u{2282}",
    "subseteq" => "\u{2286}",
    "supset" => "\u{2283}",
    "supseteq" => "\u{2287}",
    "cup" => "\u{222A}",
    "cap" => "\u{2229}",
    "setminus" => "\u{2216}",
    "forall" => "\u{2200}",
    "exists" => "\u{2203}",
    "nexists" => "\u{2204}",
    "emptyset" | "varnothing" => "\u{2205}",
    "wedge" | "land" => "\u{2227}",
    "vee" | "lor" => "\u{2228}",
    "neg" | "lnot" => "\u{00AC}",
    "oplus" => "\u{2295}",
    "otimes" => "\u{2297}",
    "odot" => "\u{2299}",
    "surd" => "\u{221A}",
    "angle" => "\u{2220}",
    "triangle" => "\u{25B3}",
    "square" => "\u{25A1}",
    "perp" => "\u{22A5}",
    "parallel" => "\u{2225}",
    "prime" => "\u{2032}",
    "ldots" | "dots" => "\u{2026}",
    "cdots" => "\u{22EF}",
    "vdots" => "\u{22EE}",
    "ddots" => "\u{22F1}",
    "aleph" => "\u{2135}",
    "hbar" => "\u{210F}",
    "ell" => "\u{2113}",
    "Re" => "\u{211C}",
    "Im" => "\u{2111}",
    "wp" => "\u{2118}",
    "leqslant" => "\u{2A7D}",
    "geqslant" => "\u{2A7E}",
    _ => return None,
  })
}
