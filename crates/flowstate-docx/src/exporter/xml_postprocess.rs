//! Post-generation `word/document.xml` rewrite seam (FS-124/125/127).
//!
//! docx-rs 0.4.20 cannot express several OOXML constructs Flowstate needs:
//! `wp:docPr` accessibility attributes (`descr`/`title`), `m:oMath` equations,
//! and `w:tblHeader`. Rather than fork docx-rs, the exporter emits deterministic
//! sentinels through the supported API and this pass rewrites them into correct
//! OOXML while the package is round-tripped in [`super::package`]:
//!
//! * Every embedded image emits an identical `<wp:docPr id="1" name="Figure" />`;
//!   [`SideChannel::image_doc_prs`] carries one entry per emitted drawing (in
//!   document order) so the k-th `docPr` gets the k-th image's alt text.
//! * Each convertible equation emits a placeholder run whose text is a unique
//!   sentinel; the enclosing `<w:r>…</w:r>` is replaced by the injected OMML.
//! * Header rows are tagged with `<w:cantSplit />` (the only per-row marker
//!   docx-rs exposes), rewritten here to `<w:tblHeader />`.
//!
//! All edits are targeted string surgery over the exact, deterministic output
//! docx-rs produces, so the rest of the document is left byte-for-byte intact.

const MATH_NAMESPACE: &str = " xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\"";
const DOC_PR_TARGET: &str = "<wp:docPr id=\"1\" name=\"Figure\" />";
const EQUATION_SENTINEL_PREFIX: &str = "@@FLOWSTATE_OMML_";
const EQUATION_SENTINEL_SUFFIX: &str = "@@";

/// Alt-text destined for one image's `wp:docPr`.
pub(super) struct ImageDocPr {
  pub(super) descr: Option<String>,
  pub(super) title: Option<String>,
}

impl ImageDocPr {
  fn render(&self, index: usize) -> String {
    // Give each image a unique docPr id so Word does not warn about the shared
    // hard-coded "1"; keep the "Figure" name docx-rs assigns.
    let mut tag = format!("<wp:docPr id=\"{}\" name=\"Figure\"", index + 1);
    if let Some(descr) = &self.descr {
      tag.push_str(" descr=\"");
      tag.push_str(&escape_attr(descr));
      tag.push('"');
    }
    if let Some(title) = &self.title {
      tag.push_str(" title=\"");
      tag.push_str(&escape_attr(title));
      tag.push('"');
    }
    tag.push_str(" />");
    tag
  }
}

/// One equation to inject: the placeholder sentinel and its OMML replacement.
pub(super) struct EquationInjection {
  sentinel: String,
  omml: String,
}

/// Data collected during block export and consumed by [`rewrite_document_xml`].
#[derive(Default)]
pub(super) struct SideChannel {
  image_doc_prs: Vec<ImageDocPr>,
  equations: Vec<EquationInjection>,
  warnings: Vec<String>,
}

impl SideChannel {
  /// Register an emitted image drawing (in document order). `descr`/`title`
  /// become the `wp:docPr` accessibility attributes when present.
  pub(super) fn push_image(&mut self, descr: Option<String>, title: Option<String>) {
    self.image_doc_prs.push(ImageDocPr { descr, title });
  }

  /// Register an equation's OMML and return the sentinel text to embed in the
  /// placeholder run so the rewrite pass can locate and replace it.
  pub(super) fn push_equation(&mut self, omml: String) -> String {
    let sentinel = format!("{EQUATION_SENTINEL_PREFIX}{}{EQUATION_SENTINEL_SUFFIX}", self.equations.len());
    self.equations.push(EquationInjection {
      sentinel: sentinel.clone(),
      omml,
    });
    sentinel
  }

  /// Record a non-fatal export degradation (e.g. an image format that could not
  /// be embedded). Surfaced through [`super::write_docx_with_report`].
  pub(super) fn push_warning(&mut self, warning: String) {
    self.warnings.push(warning);
  }

  pub(super) fn into_warnings(self) -> Vec<String> {
    self.warnings
  }
}

/// Rewrite `word/document.xml` bytes, applying every queued transform.
#[hotpath::measure]
pub(super) fn rewrite_document_xml(bytes: Vec<u8>, side: &SideChannel) -> Vec<u8> {
  let mut xml = match String::from_utf8(bytes) {
    Ok(text) => text,
    // Non-UTF-8 document.xml is not something docx-rs produces; pass through.
    Err(error) => return error.into_bytes(),
  };
  if !side.equations.is_empty() {
    xml = ensure_math_namespace(xml);
  }
  xml = rewrite_doc_prs(&xml, &side.image_doc_prs);
  // §perf: splice every equation's run in one forward pass over the xml instead
  // of rebuilding the whole String once per equation (O(N) vs O(N·equations)).
  if !side.equations.is_empty() {
    xml = inject_equations(&xml, &side.equations);
  }
  // §perf: the two whole-document `.replace()` scans below only ever match the
  // `<w:cantSplit` marker; skip both when it is absent (header rows are rare).
  if xml.contains("<w:cantSplit") {
    xml = inject_table_headers(&xml);
  }
  xml.into_bytes()
}

/// Ensure `xmlns:m` is declared on `w:document` so injected `m:oMath` resolves.
fn ensure_math_namespace(xml: String) -> String {
  let Some(start) = xml.find("<w:document") else {
    return xml;
  };
  let Some(relative_end) = xml[start..].find('>') else {
    return xml;
  };
  let end = start + relative_end;
  if xml[start..end].contains("xmlns:m=") {
    return xml;
  }
  let mut out = String::with_capacity(xml.len() + MATH_NAMESPACE.len());
  out.push_str(&xml[..end]);
  out.push_str(MATH_NAMESPACE);
  out.push_str(&xml[end..]);
  out
}

/// Replace each identical image `wp:docPr` with a per-image variant carrying
/// alt text, matched to images by document order.
fn rewrite_doc_prs(xml: &str, entries: &[ImageDocPr]) -> String {
  if entries.is_empty() || !xml.contains(DOC_PR_TARGET) {
    return xml.to_string();
  }
  let mut out = String::with_capacity(xml.len());
  let mut rest = xml;
  let mut index = 0usize;
  while let Some(pos) = rest.find(DOC_PR_TARGET) {
    out.push_str(&rest[..pos]);
    match entries.get(index) {
      Some(entry) => out.push_str(&entry.render(index)),
      None => out.push_str(DOC_PR_TARGET),
    }
    rest = &rest[pos + DOC_PR_TARGET.len()..];
    index += 1;
  }
  out.push_str(rest);
  out
}

/// Replace every placeholder run containing an equation sentinel with its OMML
/// in a single forward pass. Each sentinel occupies a run of its own, so the
/// enclosing `<w:r>…</w:r>` maps 1:1 to the equation.
///
/// §perf: the previous implementation rebuilt the whole document `String` once
/// per equation (O(N·equations)). Each equation lives in a disjoint run, so the
/// run bounds are computed against the original `xml` once, ordered by position,
/// then spliced into a single output buffer — byte-identical to the sequential
/// per-equation rewrite but linear in the document length.
fn inject_equations(xml: &str, equations: &[EquationInjection]) -> String {
  // Resolve each equation to its enclosing run span in the original xml.
  let mut spans: Vec<(usize, usize, &str)> = Vec::with_capacity(equations.len());
  for equation in equations {
    let Some(pos) = xml.find(&equation.sentinel) else {
      continue;
    };
    // docx-rs emits run opens as the exact literal `<w:r>` (no attributes) and
    // runs never nest, so the nearest enclosing tags bound the placeholder run.
    let Some(run_start) = xml[..pos].rfind("<w:r>") else {
      continue;
    };
    let Some(relative_end) = xml[pos..].find("</w:r>") else {
      continue;
    };
    let run_end = pos + relative_end + "</w:r>".len();
    spans.push((run_start, run_end, equation.omml.as_str()));
  }
  // Sentinels are unique but may be registered in any order; splicing requires
  // ascending, non-overlapping spans (runs are disjoint by construction).
  spans.sort_by_key(|(run_start, _, _)| *run_start);
  let omml_len: usize = spans.iter().map(|(_, _, omml)| omml.len()).sum();
  let mut out = String::with_capacity(xml.len() + omml_len);
  let mut cursor = 0usize;
  for (run_start, run_end, omml) in spans {
    out.push_str(&xml[cursor..run_start]);
    out.push_str(omml);
    cursor = run_end;
  }
  out.push_str(&xml[cursor..]);
  out
}

/// Rewrite header-row markers into `w:tblHeader`.
fn inject_table_headers(xml: &str) -> String {
  xml
    .replace("<w:cantSplit />", "<w:tblHeader />")
    .replace("<w:cantSplit/>", "<w:tblHeader />")
}

fn escape_attr(value: &str) -> String {
  value
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn math_namespace_added_once() {
    let xml = "<w:document xmlns:w=\"w\"><w:body/></w:document>".to_string();
    let with_ns = ensure_math_namespace(xml);
    assert!(with_ns.contains("xmlns:m="));
    let idempotent = ensure_math_namespace(with_ns.clone());
    assert_eq!(with_ns.matches("xmlns:m=").count(), idempotent.matches("xmlns:m=").count());
  }

  #[test]
  fn doc_pr_rewrite_is_ordered_and_unique() {
    let xml = format!("a{DOC_PR_TARGET}b{DOC_PR_TARGET}c");
    let entries = vec![
      ImageDocPr {
        descr: Some("first".to_string()),
        title: Some("first".to_string()),
      },
      ImageDocPr { descr: None, title: None },
    ];
    let out = rewrite_doc_prs(&xml, &entries);
    assert!(out.contains("id=\"1\" name=\"Figure\" descr=\"first\" title=\"first\" />"));
    assert!(out.contains("id=\"2\" name=\"Figure\" />"));
  }

  #[test]
  fn equation_injection_replaces_enclosing_run() {
    let sentinel = "@@FLOWSTATE_OMML_0@@";
    let xml = format!("<w:p><w:r><w:rPr/><w:t xml:space=\"preserve\">{sentinel}</w:t></w:r></w:p>");
    let out = inject_equations(
      &xml,
      &[EquationInjection {
        sentinel: sentinel.to_string(),
        omml: "<m:oMath/>".to_string(),
      }],
    );
    assert_eq!(out, "<w:p><m:oMath/></w:p>");
  }

  #[test]
  fn header_marker_becomes_tbl_header() {
    let xml = "<w:trPr><w:cantSplit /></w:trPr>";
    assert_eq!(inject_table_headers(xml), "<w:trPr><w:tblHeader /></w:trPr>");
  }
}
