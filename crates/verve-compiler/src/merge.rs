use serde_json::{Map, Value};

use crate::CompileDiagnostic;

const LIST_DIRECTIVES: [(&str, &str, DirectivePosition); 6] = [
    ("prepend-rules", "rules", DirectivePosition::Prepend),
    ("append-rules", "rules", DirectivePosition::Append),
    ("prepend-proxies", "proxies", DirectivePosition::Prepend),
    ("append-proxies", "proxies", DirectivePosition::Append),
    (
        "prepend-proxy-groups",
        "proxy-groups",
        DirectivePosition::Prepend,
    ),
    (
        "append-proxy-groups",
        "proxy-groups",
        DirectivePosition::Append,
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectivePosition {
    Prepend,
    Append,
}

/// Parses a YAML string into a JSON object (`serde_json::Map`).
///
/// When `allow_empty` is `true`, whitespace-only or YAML `null` documents yield `Ok(None)`.
pub(crate) fn parse_yaml_object(
    raw: &str,
    stage: &str,
    allow_empty: bool,
) -> Result<Option<Map<String, Value>>, CompileDiagnostic> {
    if allow_empty && raw.trim().is_empty() {
        return Ok(None);
    }

    let yaml_val: serde_yaml_ng::Value = serde_yaml_ng::from_str(raw).map_err(|err| {
        let loc = err.location();
        CompileDiagnostic {
            stage: stage.to_owned(),
            line: loc.as_ref().map(|l| l.line()),
            column: loc.as_ref().map(|l| l.column()),
            message: err.to_string(),
        }
    })?;

    if allow_empty && matches!(yaml_val, serde_yaml_ng::Value::Null) {
        return Ok(None);
    }

    let json_val = yaml_value_to_json(yaml_val, stage)?;
    match json_val {
        Value::Object(map) => Ok(Some(map)),
        other => Err(CompileDiagnostic {
            stage: stage.to_owned(),
            line: None,
            column: None,
            message: format!(
                "expected YAML root to be a mapping, found {}",
                json_kind_name(&other)
            ),
        }),
    }
}

/// Applies declarative Merge YAML (deep object merge + 6 `prepend-*` / `append-*` list directives)
/// onto `doc`.
pub(crate) fn apply_merge_yaml(
    doc: &mut Value,
    merge_yaml: &str,
    stage: &str,
) -> Result<bool, CompileDiagnostic> {
    let Some(mut overlay_map) = parse_yaml_object(merge_yaml, stage, true)? else {
        return Ok(false);
    };

    let mut pending_directives = Vec::new();
    for (directive_key, target_key, position) in LIST_DIRECTIVES {
        if let Some(val) = overlay_map.remove(directive_key) {
            let items = match val {
                Value::Array(items) => items,
                Value::Null => Vec::new(),
                other => {
                    return Err(CompileDiagnostic {
                        stage: stage.to_owned(),
                        line: None,
                        column: None,
                        message: format!(
                            "directive `{directive_key}` must be a YAML sequence, found {}",
                            json_kind_name(&other)
                        ),
                    });
                }
            };
            pending_directives.push((target_key, position, items));
        }
    }

    deep_merge_value(doc, Value::Object(overlay_map));

    let root_map = doc.as_object_mut().ok_or_else(|| CompileDiagnostic {
        stage: stage.to_owned(),
        line: None,
        column: None,
        message: "compiled document root must remain a mapping".into(),
    })?;

    for (target_key, position, items) in pending_directives {
        if items.is_empty() {
            continue;
        }
        match position {
            DirectivePosition::Prepend => prepend_list_items(root_map, target_key, items, stage)?,
            DirectivePosition::Append => append_list_items(root_map, target_key, items, stage)?,
        }
    }

    Ok(true)
}

/// Prepends `items` to `root_map[target_key]`, preserving order.
pub(crate) fn prepend_list_items(
    root_map: &mut Map<String, Value>,
    target_key: &str,
    mut items: Vec<Value>,
    stage: &str,
) -> Result<(), CompileDiagnostic> {
    let existing = extract_existing_array(root_map, target_key, stage)?;
    items.extend(existing);
    root_map.insert(target_key.to_owned(), Value::Array(items));
    Ok(())
}

/// Appends `items` to `root_map[target_key]`, preserving order.
pub(crate) fn append_list_items(
    root_map: &mut Map<String, Value>,
    target_key: &str,
    items: Vec<Value>,
    stage: &str,
) -> Result<(), CompileDiagnostic> {
    let mut existing = extract_existing_array(root_map, target_key, stage)?;
    existing.extend(items);
    root_map.insert(target_key.to_owned(), Value::Array(existing));
    Ok(())
}

fn extract_existing_array(
    root_map: &mut Map<String, Value>,
    target_key: &str,
    stage: &str,
) -> Result<Vec<Value>, CompileDiagnostic> {
    match root_map.remove(target_key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(arr)) => Ok(arr),
        Some(other) => Err(CompileDiagnostic {
            stage: stage.to_owned(),
            line: None,
            column: None,
            message: format!(
                "target field `{target_key}` must be a YAML sequence, found {}",
                json_kind_name(&other)
            ),
        }),
    }
}

fn deep_merge_value(target: &mut Value, overlay: Value) {
    match (target, overlay) {
        (Value::Object(target_map), Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                if let Some(existing) = target_map.get_mut(&key) {
                    deep_merge_value(existing, value);
                } else {
                    target_map.insert(key, value);
                }
            }
        }
        (slot, replacement) => {
            *slot = replacement;
        }
    }
}

fn yaml_value_to_json(val: serde_yaml_ng::Value, stage: &str) -> Result<Value, CompileDiagnostic> {
    match val {
        serde_yaml_ng::Value::Null => Ok(Value::Null),
        serde_yaml_ng::Value::Bool(b) => Ok(Value::Bool(b)),
        serde_yaml_ng::Value::Number(num) => {
            if let Some(u) = num.as_u64() {
                Ok(Value::Number(u.into()))
            } else if let Some(i) = num.as_i64() {
                Ok(Value::Number(i.into()))
            } else if let Some(f) = num.as_f64() {
                let json_num =
                    serde_json::Number::from_f64(f).ok_or_else(|| CompileDiagnostic {
                        stage: stage.to_owned(),
                        line: None,
                        column: None,
                        message: format!("non-finite YAML floating-point number `{f}`"),
                    })?;
                Ok(Value::Number(json_num))
            } else {
                Err(CompileDiagnostic {
                    stage: stage.to_owned(),
                    line: None,
                    column: None,
                    message: "unsupported YAML number representation".into(),
                })
            }
        }
        serde_yaml_ng::Value::String(s) => Ok(Value::String(s)),
        serde_yaml_ng::Value::Sequence(seq) => {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq {
                out.push(yaml_value_to_json(item, stage)?);
            }
            Ok(Value::Array(out))
        }
        serde_yaml_ng::Value::Mapping(mapping) => {
            let mut out = Map::with_capacity(mapping.len());
            for (raw_key, raw_val) in mapping {
                let key = yaml_key_to_string(raw_key, stage)?;
                let val = yaml_value_to_json(raw_val, stage)?;
                out.insert(key, val);
            }
            Ok(Value::Object(out))
        }
        serde_yaml_ng::Value::Tagged(tagged) => yaml_value_to_json(tagged.value, stage),
    }
}

fn yaml_key_to_string(key: serde_yaml_ng::Value, stage: &str) -> Result<String, CompileDiagnostic> {
    match key {
        serde_yaml_ng::Value::String(s) => Ok(s),
        serde_yaml_ng::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml_ng::Value::Number(n) => Ok(n.to_string()),
        serde_yaml_ng::Value::Null => Ok("null".into()),
        serde_yaml_ng::Value::Tagged(tagged) => yaml_key_to_string(tagged.value, stage),
        other => Err(CompileDiagnostic {
            stage: stage.to_owned(),
            line: None,
            column: None,
            message: format!("YAML mapping key must be a scalar, found {other:?}"),
        }),
    }
}

fn json_kind_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "sequence",
        Value::Object(_) => "mapping",
    }
}
