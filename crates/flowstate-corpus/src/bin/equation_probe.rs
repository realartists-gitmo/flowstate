//! B-S9: the equation fidelity net's MEASUREMENT half. Walks the real debate
//! corpus, harvests every `m:oMath`/`m:oMathPara` from `word/document.xml`,
//! drives the import converter (OMML → LaTeX) and then the export converter
//! (LaTeX → OMML) over the harvested population, and reports the
//! `[Equation: …]` fallback rate — THE metric — plus the top unconvertible
//! sources so converter widening chases real holes, not guesses. A synthetic
//! construct battery runs alongside (corpus math skews simple; synthetics
//! keep the rare constructs measured).
//!
//! Usage: `cargo run -p flowstate-corpus --release --bin equation_probe [corpus_dir] [--jobs N]`

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Tally {
  files_scanned: usize,
  files_with_math: usize,
  omath_spans: usize,
  imported_equations: usize,
  import_dropped_spans: usize,
  export_fallbacks: usize,
  fallback_sources: BTreeMap<String, usize>,
}

fn main() {
  let mut args = std::env::args().skip(1);
  let mut corpus_dir: Option<PathBuf> = None;
  let mut jobs = 8usize;
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--jobs" => jobs = args.next().and_then(|v| v.parse().ok()).unwrap_or(8),
      other => corpus_dir = Some(PathBuf::from(other)),
    }
  }
  let corpus_dir = corpus_dir
    .or_else(|| std::env::var_os("FLOWSTATE_CORPUS_DIR").map(PathBuf::from))
    .unwrap_or_else(|| PathBuf::from("helpers/corpus/dropbox-office-flat"));

  let mut files: Vec<PathBuf> = std::fs::read_dir(&corpus_dir)
    .unwrap_or_else(|error| panic!("corpus dir {} unreadable: {error}", corpus_dir.display()))
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .filter(|path| {
      path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("docx"))
    })
    .collect();
  files.sort();
  eprintln!("equation_probe: {} .docx files, {jobs} jobs", files.len());

  let tally = Mutex::new(Tally::default());
  let next = AtomicUsize::new(0);
  std::thread::scope(|scope| {
    for _ in 0..jobs.max(1) {
      scope.spawn(|| {
        loop {
          let ix = next.fetch_add(1, Ordering::Relaxed);
          let Some(path) = files.get(ix) else { break };
          if ix.is_multiple_of(1000) {
            eprintln!("… {ix}/{}", files.len());
          }
          let local = probe_file(path);
          let mut tally = tally.lock().expect("tally lock");
          tally.files_scanned += 1;
          tally.files_with_math += usize::from(local.omath_spans > 0);
          tally.omath_spans += local.omath_spans;
          tally.imported_equations += local.imported_equations;
          tally.import_dropped_spans += local.import_dropped_spans;
          tally.export_fallbacks += local.export_fallbacks;
          for (source, count) in local.fallback_sources {
            *tally.fallback_sources.entry(source).or_default() += count;
          }
        }
      });
    }
  });
  let tally = tally.into_inner().expect("tally");

  println!("== equation_probe: corpus ==");
  println!("files scanned:          {}", tally.files_scanned);
  println!("files with math:        {}", tally.files_with_math);
  println!("oMath spans harvested:  {}", tally.omath_spans);
  println!("equations imported:     {}", tally.imported_equations);
  println!(
    "import-dropped spans:   {} ({:.2}%)",
    tally.import_dropped_spans,
    percent(tally.import_dropped_spans, tally.omath_spans)
  );
  println!(
    "export fallbacks:       {} ({:.2}%)  <-- THE metric",
    tally.export_fallbacks,
    percent(tally.export_fallbacks, tally.imported_equations)
  );
  let mut worst: Vec<(&String, &usize)> = tally.fallback_sources.iter().collect();
  worst.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
  if !worst.is_empty() {
    println!("top unconvertible sources:");
    for (source, count) in worst.iter().take(20) {
      println!("  {count:>5}×  {source:?}");
    }
  }

  println!("== equation_probe: synthetics ==");
  let mut synthetic_failures = 0usize;
  for source in SYNTHETIC_LATEX {
    let round = flowstate_docx::omml_from_latex_probe(source, true);
    if round.is_none() {
      synthetic_failures += 1;
      println!("  FALLBACK  {source:?}");
      continue;
    }
    // Round-trip: export → reimport must keep a nonempty source.
    let omml = round.expect("checked above");
    let reimported = flowstate_docx::equations_from_omml_bytes(omml.as_bytes());
    if reimported.is_empty() || reimported[0].source.trim().is_empty() {
      synthetic_failures += 1;
      println!("  REIMPORT-DROP  {source:?}");
    }
  }
  println!(
    "synthetics: {}/{} clean",
    SYNTHETIC_LATEX.len() - synthetic_failures,
    SYNTHETIC_LATEX.len()
  );
}

fn percent(part: usize, whole: usize) -> f64 {
  if whole == 0 { 0.0 } else { part as f64 * 100.0 / whole as f64 }
}

fn probe_file(path: &PathBuf) -> Tally {
  let mut local = Tally::default();
  let Ok(file) = std::fs::File::open(path) else {
    return local;
  };
  let Ok(mut archive) = zip::ZipArchive::new(file) else {
    return local;
  };
  let Ok(mut entry) = archive.by_name("word/document.xml") else {
    return local;
  };
  let mut xml = Vec::new();
  if entry.read_to_end(&mut xml).is_err() {
    return local;
  }
  for span in omath_spans(&xml) {
    local.omath_spans += 1;
    let imported = flowstate_docx::equations_from_omml_bytes(span);
    if imported.is_empty() {
      local.import_dropped_spans += 1;
      continue;
    }
    for equation in imported {
      local.imported_equations += 1;
      let display = matches!(equation.display, flowstate_document::InputEquationDisplay::Display);
      if flowstate_docx::omml_from_latex_probe(&equation.source, display).is_none() {
        local.export_fallbacks += 1;
        let mut key = equation.source.clone();
        key.truncate(120);
        *local.fallback_sources.entry(key).or_default() += 1;
      }
    }
  }
  local
}

/// Balanced `m:oMathPara` spans first (blanked so their inner `m:oMath`
/// aren't double-counted), then bare `m:oMath` spans.
fn omath_spans(xml: &[u8]) -> Vec<&[u8]> {
  let mut spans = Vec::new();
  let mut consumed: Vec<(usize, usize)> = Vec::new();
  for (open_tag, close_tag) in [
    (b"<m:oMathPara".as_slice(), b"</m:oMathPara>".as_slice()),
    (b"<m:oMath".as_slice(), b"</m:oMath>".as_slice()),
  ] {
    let mut from = 0;
    while let Some(start_rel) = find_bytes(&xml[from..], open_tag) {
      let start = from + start_rel;
      if consumed.iter().any(|(lo, hi)| start >= *lo && start < *hi) {
        from = start + open_tag.len();
        continue;
      }
      // `<m:oMath` must not match `<m:oMathPara` in the second pass.
      let after = xml.get(start + open_tag.len()).copied();
      if open_tag == b"<m:oMath" && after == Some(b'P') {
        from = start + open_tag.len();
        continue;
      }
      let Some(end_rel) = find_bytes(&xml[start..], close_tag) else {
        break;
      };
      let end = start + end_rel + close_tag.len();
      spans.push(&xml[start..end]);
      consumed.push((start, end));
      from = end;
    }
  }
  spans
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
  haystack
    .windows(needle.len())
    .position(|window| window == needle)
}

/// The synthetic construct battery: the documented converter surface plus
/// the constructs debate evidence actually contains.
const SYNTHETIC_LATEX: &[&str] = &[
  "x^2 + y^2 = z^2",
  "\\frac{a}{b}",
  "\\frac{\\partial f}{\\partial x}",
  "\\sqrt{x + 1}",
  "\\sqrt[3]{x}",
  "\\sum_{i=1}^{n} i^2",
  "\\int_0^\\infty e^{-x} dx",
  "\\prod_{k=1}^{n} k",
  "\\lim_{x \\to 0} \\frac{\\sin x}{x}",
  "a_1 + a_2 + \\dots + a_n",
  "\\alpha \\beta \\gamma \\delta \\pi \\sigma \\omega",
  "\\left( \\frac{a}{b} \\right)",
  "\\begin{matrix} a & b \\\\ c & d \\end{matrix}",
  "\\hat{x} + \\bar{y} + \\vec{v}",
  "x \\leq y \\geq z \\neq w \\approx q",
  "\\log_2 n + \\ln x + \\sin \\theta",
  "90\\%",
  "CO_2",
  "E = mc^2",
  "\\frac{dN}{dt} = rN(1 - \\frac{N}{K})",
];
