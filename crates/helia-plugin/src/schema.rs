// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The JSON Schema subset used for plugin settings and step parameters.
//!
//! Enough to draw a form and check what the user typed: an object with `properties` of type
//! `string`, `number`, `integer` or `boolean`, with `title`, `description`, `default`, `enum`,
//! `minimum`, `maximum`, `minLength`, `maxLength`, `format` (`password`, `path`), `x-secret`,
//! `x-order` and the object's `required`. Everything else is ignored. A schema is untrusted
//! input: sizes and counts are bounded.

use serde_json::{Map, Value};

/// Most properties in one schema.
pub const MAX_FIELDS: usize = 64;
/// Longest text value accepted when the schema gives no `maxLength`.
pub const MAX_TEXT: usize = 16 * 1024;

/// How a field is drawn and checked.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    /// Free text.
    Text,
    /// Hidden text, kept in the approvals file and never logged.
    Secret,
    /// A file or folder path.
    Path,
    /// A number; `integer` rounds, the range gives a slider.
    Number {
        /// Smallest value.
        min: Option<f64>,
        /// Largest value.
        max: Option<f64>,
        /// Whole numbers only.
        integer: bool,
    },
    /// A yes or no.
    Bool,
    /// One of a list of values.
    Choice(Vec<Value>),
}

/// One field of the form.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Key in the settings object.
    pub key: String,
    /// Label (the `title`, else the key).
    pub label: String,
    /// One line of help.
    pub help: String,
    /// Kind.
    pub kind: FieldKind,
    /// Initial value.
    pub default: Option<Value>,
    /// Must be present.
    pub required: bool,
    /// Text length limits.
    pub min_len: usize,
    /// Text length limits.
    pub max_len: usize,
}

/// A parsed schema.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    /// The fields, in `x-order` then name order.
    pub fields: Vec<Field>,
}

impl Schema {
    /// Parse a JSON Schema document.
    ///
    /// # Errors
    /// A sentence saying what is unusable (too many properties, a property that is not an object).
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let Some(props) = v.get("properties") else {
            return Ok(Self::default());
        };
        let Some(props) = props.as_object() else {
            return Err("properties must be an object".into());
        };
        if props.len() > MAX_FIELDS {
            return Err(format!("more than {MAX_FIELDS} properties"));
        }
        let required: Vec<&str> = v
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        for (key, p) in props {
            if key.is_empty() || key.len() > 64 {
                return Err("a property name is empty or longer than 64 bytes".into());
            }
            let Some(p) = p.as_object() else {
                return Err(format!("property {key} is not an object"));
            };
            fields.push((order(p), field(key, p, required.contains(&key.as_str()))?));
        }
        fields.sort_by(|a, b| (a.0, &a.1.key).cmp(&(b.0, &b.1.key)));
        Ok(Self {
            fields: fields.into_iter().map(|(_, f)| f).collect(),
        })
    }

    /// The settings object made of the defaults.
    #[must_use]
    pub fn defaults(&self) -> Value {
        let mut m = Map::new();
        for f in &self.fields {
            if let Some(d) = &f.default {
                m.insert(f.key.clone(), d.clone());
            }
        }
        Value::Object(m)
    }

    /// Check `v` (an object) and return it with defaults filled in and unknown keys dropped.
    ///
    /// # Errors
    /// One sentence per problem, each naming the field by its label.
    pub fn validate(&self, v: &Value) -> Result<Value, Vec<String>> {
        let empty = Map::new();
        if self.fields.is_empty() {
            // No schema: anything the user gave is passed through.
            return match v {
                Value::Object(_) => Ok(v.clone()),
                Value::Null => Ok(Value::Object(empty)),
                _ => Err(vec!["The settings must be an object.".into()]),
            };
        }
        let given = match v {
            Value::Object(m) => m,
            Value::Null => &empty,
            _ => return Err(vec!["The settings must be an object.".into()]),
        };
        let mut out = Map::new();
        let mut problems = Vec::new();
        for f in &self.fields {
            let val = given
                .get(&f.key)
                .filter(|x| !x.is_null())
                .or(f.default.as_ref());
            match val {
                None if f.required => problems.push(format!("{} is needed.", f.label)),
                None => {}
                Some(x) => match check(f, x) {
                    Ok(x) => {
                        out.insert(f.key.clone(), x);
                    }
                    Err(e) => problems.push(format!("{}: {e}", f.label)),
                },
            }
        }
        if problems.is_empty() {
            Ok(Value::Object(out))
        } else {
            Err(problems)
        }
    }
}

fn order(p: &Map<String, Value>) -> i64 {
    p.get("x-order").and_then(Value::as_i64).unwrap_or(i64::MAX)
}

fn text(p: &Map<String, Value>, k: &str, max: usize) -> String {
    let s = p.get(k).and_then(Value::as_str).unwrap_or("");
    helia_plugin_sdk::clip(s, max)
}

fn field(key: &str, p: &Map<String, Value>, required: bool) -> Result<Field, String> {
    let ty = p.get("type").and_then(Value::as_str).unwrap_or("string");
    let format = p.get("format").and_then(Value::as_str).unwrap_or("");
    let secret =
        p.get("x-secret").and_then(Value::as_bool).unwrap_or(false) || format == "password";
    let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
    let kind = if let Some(Value::Array(e)) = p.get("enum") {
        if e.is_empty() || e.len() > 256 {
            return Err(format!("property {key} has an enum of {} values", e.len()));
        }
        FieldKind::Choice(e.clone())
    } else {
        match ty {
            "string" if secret => FieldKind::Secret,
            "string" if format == "path" => FieldKind::Path,
            "string" => FieldKind::Text,
            "number" | "integer" => FieldKind::Number {
                min: num("minimum"),
                max: num("maximum"),
                integer: ty == "integer",
            },
            "boolean" => FieldKind::Bool,
            other => return Err(format!("property {key} has the unsupported type {other}")),
        }
    };
    Ok(Field {
        key: key.to_owned(),
        label: if p.contains_key("title") {
            text(p, "title", 120)
        } else {
            key.to_owned()
        },
        help: text(p, "description", 400),
        kind,
        default: p.get("default").cloned(),
        required,
        min_len: p
            .get("minLength")
            .and_then(Value::as_u64)
            .map_or(0, |n| n.min(MAX_TEXT as u64) as usize),
        max_len: p
            .get("maxLength")
            .and_then(Value::as_u64)
            .map_or(MAX_TEXT, |n| n.min(MAX_TEXT as u64) as usize),
    })
}

fn check(f: &Field, x: &Value) -> Result<Value, String> {
    match &f.kind {
        FieldKind::Text | FieldKind::Secret | FieldKind::Path => {
            let Some(s) = x.as_str() else {
                return Err("must be text".into());
            };
            if s.len() > f.max_len {
                return Err(format!("is longer than {} characters", f.max_len));
            }
            if s.len() < f.min_len {
                return Err(format!("is shorter than {} characters", f.min_len));
            }
            if s.contains('\0') {
                return Err("contains an invalid character".into());
            }
            Ok(x.clone())
        }
        FieldKind::Number { min, max, integer } => {
            let Some(n) = x.as_f64().filter(|n| n.is_finite()) else {
                return Err("must be a number".into());
            };
            if *integer && n.fract() != 0.0 {
                return Err("must be a whole number".into());
            }
            if min.is_some_and(|m| n < m) {
                return Err(format!("must be at least {}", min.unwrap_or_default()));
            }
            if max.is_some_and(|m| n > m) {
                return Err(format!("must be at most {}", max.unwrap_or_default()));
            }
            Ok(if *integer {
                Value::from(n as i64)
            } else {
                x.clone()
            })
        }
        FieldKind::Bool => x
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "must be yes or no".into()),
        FieldKind::Choice(options) => {
            if options.contains(x) {
                Ok(x.clone())
            } else {
                Err("is not one of the choices".into())
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Schema {
        Schema::from_json(&json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {"type": "string", "title": "Server", "x-order": 1},
                "token": {"type": "string", "format": "password", "x-order": 2},
                "n": {"type": "integer", "minimum": 1, "maximum": 10, "default": 3},
                "mode": {"enum": ["a", "b"], "default": "a"},
                "on": {"type": "boolean"}
            }
        }))
        .unwrap()
    }

    #[test]
    fn order_and_kinds() {
        let s = schema();
        assert_eq!(s.fields[0].key, "url");
        assert_eq!(s.fields[1].kind, FieldKind::Secret);
        assert!(matches!(
            s.fields.iter().find(|f| f.key == "mode").unwrap().kind,
            FieldKind::Choice(_)
        ));
    }

    #[test]
    fn validates_and_fills() {
        let s = schema();
        let v = s.validate(&json!({"url": "x", "extra": 1})).unwrap();
        assert_eq!(v, json!({"url": "x", "n": 3, "mode": "a"}));
        let e = s.validate(&json!({"n": 99, "mode": "z"})).unwrap_err();
        assert_eq!(e.len(), 3);
        assert!(s.validate(&json!([1])).is_err());
        assert!(s.validate(&json!({"url": 5})).is_err());
        assert!(s.validate(&json!({"url": "x", "n": 1.5})).is_err());
    }

    #[test]
    fn hostile_schemas() {
        assert!(Schema::from_json(&json!({"properties": []})).is_err());
        assert!(Schema::from_json(&json!({"properties": {"a": 1}})).is_err());
        assert!(Schema::from_json(&json!({"properties": {"a": {"type": "object"}}})).is_err());
        let mut m = Map::new();
        for i in 0..100 {
            m.insert(format!("k{i}"), json!({"type": "string"}));
        }
        assert!(Schema::from_json(&json!({"properties": m})).is_err());
    }
}
