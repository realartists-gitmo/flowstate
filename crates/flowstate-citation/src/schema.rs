//! Strict validation for the sparse citation JSON contract.
//!
//! Parsing as `serde_json::Value` only proves syntactic JSON. Model output must also match the
//! frozen field/type/enumeration contract before it can be treated as a successful citation.

use serde_json::{Map, Value};

const PARSED_KEYS: &[&str] = &[
    "status",
    "authors",
    "year",
    "no_date",
    "published_date",
    "accessed_date",
    "retrieved_date",
    "title",
    "container_title",
    "publication",
    "publisher",
    "volume",
    "issue",
    "pages",
    "url",
    "doi",
    "database",
    "source_type",
    "card_signatures",
    "debate_annotations",
    "raw_tail",
    "spillover_start_index",
    "spillover_start_text",
    "warnings",
];

const REJECT_KEYS: &[&str] = &["status", "reject_reason", "evidence", "warnings"];
const AUTHOR_KEYS: &[&str] = &["surname", "name", "qualifications"];

const SOURCE_TYPES: &[&str] = &[
    "journal_article",
    "law_review",
    "news_article",
    "web_page",
    "book",
    "book_chapter",
    "report",
    "thesis",
    "legal_source",
    "dictionary_or_reference",
    "interview",
    "unknown",
];

const REJECT_REASONS: &[&str] = &[
    "not_a_citation",
    "analytic_or_tag",
    "cross_reference_only",
    "too_malformed",
    "empty_or_placeholder",
];

const WARNINGS: &[&str] = &[
    "incomplete_citation",
    "url_only",
    "conflicting_dates",
    "source_type_ambiguous",
    "et_al",
    "no_date",
    "body_spillover",
];

const STRING_FIELDS: &[&str] = &[
    "published_date",
    "accessed_date",
    "retrieved_date",
    "title",
    "container_title",
    "publication",
    "publisher",
    "volume",
    "issue",
    "pages",
    "url",
    "doi",
    "database",
    "raw_tail",
    "spillover_start_text",
];

const STRING_ARRAY_FIELDS: &[&str] = &["card_signatures", "debate_annotations"];

fn has_only_keys(map: &Map<String, Value>, allowed: &[&str]) -> bool {
    map.keys().all(|key| allowed.contains(&key.as_str()))
}

fn nonempty_string(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| !text.trim().is_empty())
}

fn identity_text(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn valid_string_array(value: &Value, allowed: Option<&[&str]>) -> bool {
    value.as_array().is_some_and(|items| {
        !items.is_empty()
            && items.iter().all(|item| {
                item.as_str().is_some_and(|text| {
                    !text.trim().is_empty()
                        && allowed.is_none_or(|values| values.contains(&text))
                })
            })
    })
}

fn validate_authors(value: &Value) -> Option<&'static str> {
    let Some(authors) = value.as_array() else {
        return Some("schema_invalid_authors");
    };
    if authors.is_empty() {
        return Some("schema_invalid_authors");
    }
    for author in authors {
        let Some(map) = author.as_object() else {
            return Some("schema_invalid_author");
        };
        let surname = map.get("surname").and_then(Value::as_str);
        let name = map.get("name").and_then(Value::as_str);
        if !has_only_keys(map, AUTHOR_KEYS)
            || surname.is_none_or(|text| text.trim().is_empty())
            || name.is_none_or(|text| text.trim().is_empty())
            || !name.is_some_and(|name| {
                surname.is_some_and(|surname| identity_text(name).contains(&identity_text(surname)))
            })
        {
            return Some("schema_invalid_author");
        }
        if let Some(qualifications) = map.get("qualifications") {
            let Some(items) = qualifications.as_array() else {
                return Some("schema_invalid_qualifications");
            };
            if items.len() > 1
                || items
                    .iter()
                    .any(|item| !nonempty_string(Some(item)))
            {
                return Some("schema_invalid_qualifications");
            }
        }
    }
    None
}

fn validate_warnings(map: &Map<String, Value>) -> Option<&'static str> {
    if map
        .get("warnings")
        .is_some_and(|warnings| !valid_string_array(warnings, Some(WARNINGS)))
    {
        return Some("schema_invalid_warnings");
    }
    None
}

fn validate_parsed(map: &Map<String, Value>) -> Option<&'static str> {
    if !has_only_keys(map, PARSED_KEYS) {
        return Some("schema_unknown_field");
    }
    let source_type = map.get("source_type").and_then(Value::as_str);
    if !source_type.is_some_and(|kind| SOURCE_TYPES.contains(&kind)) {
        return Some("schema_invalid_source_type");
    }
    if let Some(authors) = map.get("authors")
        && let Some(reason) = validate_authors(authors)
    {
        return Some(reason);
    }
    if STRING_FIELDS
        .iter()
        .any(|key| map.get(*key).is_some_and(|value| !nonempty_string(Some(value))))
    {
        return Some("schema_invalid_string_field");
    }
    if STRING_ARRAY_FIELDS
        .iter()
        .any(|key| map.get(*key).is_some_and(|value| !valid_string_array(value, None)))
    {
        return Some("schema_invalid_string_array");
    }
    if map.get("year").is_some_and(|year| {
        !year
            .as_i64()
            .is_some_and(|year| (1_000..=2_099).contains(&year))
    }) {
        return Some("schema_invalid_year");
    }
    if map.get("no_date").is_some_and(|value| value.as_bool() != Some(true))
        || (map.contains_key("no_date") && map.contains_key("year"))
    {
        return Some("schema_invalid_date");
    }
    if map
        .get("spillover_start_index")
        .is_some_and(|index| index.as_u64().is_none())
    {
        return Some("schema_invalid_spillover");
    }
    validate_warnings(map)
}

fn validate_reject(map: &Map<String, Value>) -> Option<&'static str> {
    if !has_only_keys(map, REJECT_KEYS) {
        return Some("schema_unknown_field");
    }
    let reason = map.get("reject_reason").and_then(Value::as_str);
    if !reason.is_some_and(|reason| REJECT_REASONS.contains(&reason)) {
        return Some("schema_invalid_reject_reason");
    }
    if map
        .get("evidence")
        .is_some_and(|evidence| !nonempty_string(Some(evidence)))
    {
        return Some("schema_invalid_evidence");
    }
    validate_warnings(map)
}

/// Return the first schema-contract violation, or `None` for a valid sparse citation object.
pub fn fails(value: &Value) -> Option<&'static str> {
    let Some(map) = value.as_object() else {
        return Some("schema_not_object");
    };
    match map.get("status").and_then(Value::as_str) {
        Some("parsed") => validate_parsed(map),
        Some("reject") => validate_reject(map),
        _ => Some("schema_invalid_status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_sparse_parsed_and_reject_objects() {
        let parsed = json!({
            "status": "parsed",
            "authors": [{"surname": "Moten", "name": "Fred Moten"}],
            "source_type": "interview"
        });
        let reject = json!({
            "status": "reject",
            "reject_reason": "not_a_citation",
            "evidence": "not a source"
        });
        assert_eq!(fails(&parsed), None);
        assert_eq!(fails(&reject), None);
    }

    #[test]
    fn rejects_json_that_does_not_match_the_contract() {
        let unknown = json!({"status": "parsed", "source_type": "report", "surprise": 1});
        let incomplete_author = json!({
            "status": "parsed",
            "source_type": "report",
            "authors": [{"surname": "Toon"}]
        });
        let wrong_type = json!({"status": "parsed", "source_type": "report", "year": "2020"});

        assert_eq!(fails(&unknown), Some("schema_unknown_field"));
        assert_eq!(fails(&incomplete_author), Some("schema_invalid_author"));
        assert_eq!(fails(&wrong_type), Some("schema_invalid_year"));
    }
}
