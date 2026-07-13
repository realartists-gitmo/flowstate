use serde::Deserialize;
use serde_json::{Value, json};

use super::host_error;
use crate::bindings::flowstate::extension::host;

#[derive(Deserialize)]
struct Snapshot {
    generation: u64,
    document: Document,
    selection: Selection,
}

#[derive(Deserialize)]
struct Document {
    blocks: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Selection {
    Text { anchor: Offset, head: Offset },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, serde::Serialize)]
struct Offset {
    paragraph: usize,
    byte: usize,
}

fn snapshot() -> Result<Snapshot, String> {
    let source = host::snapshot().map_err(host_error)?;
    serde_json::from_str(&source).map_err(|error| format!("invalid snapshot: {error}"))
}

pub fn inspect() -> Result<(), String> {
    let snapshot = host::snapshot().map_err(host_error)?;
    let selection = host::selection().map_err(host_error)?;
    host::set_status(&format!("snapshot={} bytes; selection={selection}", snapshot.len()));
    host::set_action_label("inspect", "Inspect again").map_err(host_error)
}

pub fn replace_selection() -> Result<(), String> {
    let snapshot = snapshot()?;
    let Selection::Text { anchor, head } = snapshot.selection else {
        return Err("select text before running this action".into());
    };
    let request = json!({
        "expected_generation": snapshot.generation,
        "edits": [{
            "kind": "replace_text",
            "start": anchor,
            "end": head,
            "fragment": styled_fragment("Replaced by the example extension"),
        }],
    });
    host::apply_edits(&request.to_string()).map_err(host_error)?;
    Ok(())
}

fn styled_fragment(text: &str) -> Value {
    json!({
        "format": "gpui-flowtext.rich-text-fragment.v1",
        "paragraphs": [{
            "style": "Normal",
            "runs": [{ "text": text, "styles": {
                "semantic": { "Custom": 1 }, "direct_underline": true,
                "strikethrough": false, "highlight": { "Custom": 1 }
            }}]
        }],
        "blocks": [], "assets": []
    })
}

pub fn refresh() -> Result<(), String> {
    host::refresh_from_disk().map_err(host_error)?;
    Ok(())
}
