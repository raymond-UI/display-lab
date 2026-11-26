//! Argument templates.
//!
//! A tool's `args` in the config are JSON with placeholders. A string that is
//! exactly `"$name"` or `"$name.field.sub"` is replaced by that variable (or a
//! field inside it), keeping its JSON type: `"$width"` becomes the number
//! 1920, not the string "1920". Anything else is kept as written, and `"$$x"`
//! is the literal string `"$x"`.

use serde_json::{Map, Value};

pub type Vars = Map<String, Value>;

pub fn render(template: &Value, vars: &Vars) -> Result<Value, String> {
    Ok(match template {
        Value::String(s) => match s.strip_prefix('$') {
            Some(rest) if rest.starts_with('$') => Value::String(rest.to_string()),
            Some(path) => lookup(vars, path)?,
            None => template.clone(),
        },
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| render(v, vars))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), render(v, vars)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn lookup(vars: &Vars, path: &str) -> Result<Value, String> {
    let mut parts = path.split('.');
    let head = parts.next().unwrap_or_default();
    let mut value = vars.get(head).ok_or_else(|| {
        let known: Vec<&str> = vars.keys().map(String::as_str).collect();
        format!("${head} is not set here (known: {})", known.join(", "))
    })?;
    for part in parts {
        value = value
            .get(part)
            .ok_or_else(|| format!("${path}: no field {part:?} in {value}"))?;
    }
    Ok(value.clone())
}

/// `pointer` into `value`, treating JSON null as missing.
pub fn at<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    value.pointer(pointer).filter(|v| !v.is_null())
}

/// A number that may arrive as a JSON number or as a digit string (the
/// protobuf JSON mapping writes 64-bit integers as strings).
pub fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> Vars {
        let mut v = Vars::new();
        v.insert("width".into(), json!(1920));
        v.insert("output".into(), json!({"id": "101", "kind": "AUDIENCE"}));
        v
    }

    #[test]
    fn placeholders_keep_their_type() {
        let out = render(
            &json!({"output": "$output.id", "w": "$width", "label": "w$width", "lit": "$$width", "list": ["$width", 2]}),
            &vars(),
        )
        .unwrap();
        assert_eq!(
            out,
            json!({"output": "101", "w": 1920, "label": "w$width", "lit": "$width", "list": [1920, 2]})
        );
    }

    #[test]
    fn missing_variables_are_named() {
        let e = render(&json!("$slide"), &vars()).unwrap_err();
        assert!(e.contains("$slide") && e.contains("width"), "{e}");
        assert!(render(&json!("$output.nope"), &vars()).is_err());
    }

    #[test]
    fn numbers_from_strings() {
        assert_eq!(as_i64(&json!("42")), Some(42));
        assert_eq!(as_i64(&json!(-3)), Some(-3));
        assert_eq!(as_i64(&json!(true)), None);
        assert!(at(&json!({"a": null}), "/a").is_none());
    }
}
