use crate::ir::Api;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("YAML parse error at line {line}, column {column}: {message}")]
    Yaml {
        line: usize,
        column: usize,
        message: String,
    },
    #[error("TOML parse error: {message}")]
    Toml { message: String },
    #[error("JSON parse error at line {line}, column {column}: {message}")]
    Json {
        line: usize,
        column: usize,
        message: String,
    },
}

pub fn parse_api_str(s: &str, format: &str) -> Result<Api, ParseError> {
    match format {
        "yaml" | "yml" => serde_yaml::from_str(s).map_err(|e| {
            let (line, column) = e
                .location()
                .map(|m| (m.line(), m.column()))
                .unwrap_or((0, 0));
            ParseError::Yaml {
                line,
                column,
                message: e.to_string(),
            }
        }),
        "json" => serde_json::from_str(s).map_err(|e| ParseError::Json {
            line: e.line(),
            column: e.column(),
            message: e.to_string(),
        }),
        "toml" => toml::from_str(s).map_err(|e| ParseError::Toml {
            message: e.to_string(),
        }),
        other => Err(ParseError::UnsupportedFormat(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Api, Function, Module, Param, TypeRef};

    fn expected_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: Some("Adds two numbers".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        }
    }

    #[test]
    fn parse_yaml_round_trip() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - name: a
            type: i32
          - name: b
            type: i32
        return: i32
        doc: "Adds two numbers"
"#;
        assert_eq!(parse_api_str(yaml, "yaml").unwrap(), expected_api());
    }

    #[test]
    fn parse_json_round_trip() {
        let json = r#"{
            "version": "0.1.0",
            "modules": [{
                "name": "math",
                "functions": [{
                    "name": "add",
                    "params": [
                        {"name": "a", "type": "i32"},
                        {"name": "b", "type": "i32"}
                    ],
                    "return": "i32",
                    "doc": "Adds two numbers"
                }]
            }]
        }"#;
        assert_eq!(parse_api_str(json, "json").unwrap(), expected_api());
    }

    #[test]
    fn parse_toml_round_trip() {
        let toml_str = r#"
version = "0.1.0"

[[modules]]
name = "math"

[[modules.functions]]
name = "add"
return = "i32"
doc = "Adds two numbers"

[[modules.functions.params]]
name = "a"
type = "i32"

[[modules.functions.params]]
name = "b"
type = "i32"
"#;
        assert_eq!(parse_api_str(toml_str, "toml").unwrap(), expected_api());
    }

    #[test]
    fn parse_missing_optional_fields() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions:
      - name: noop
        params: []
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.returns, None);
        assert_eq!(f.doc, None);
        assert!(!f.r#async);
        assert_eq!(api.modules[0].errors, None);
    }

    #[test]
    fn unsupported_format_returns_error() {
        let err = parse_api_str("{}", "xml").unwrap_err();
        assert!(matches!(err, ParseError::UnsupportedFormat(f) if f == "xml"));
    }

    #[test]
    fn parse_struct_definitions() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Person
        fields:
          - name: name
            type: string
          - name: age
            type: i32
          - name: active
            type: bool
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let s = &api.modules[0].structs[0];
        assert_eq!(s.name, "Person");
        assert_eq!(s.fields.len(), 3);
        assert_eq!(s.fields[0].name, "name");
        assert_eq!(s.fields[0].ty, TypeRef::StringUtf8);
        assert_eq!(s.fields[1].name, "age");
        assert_eq!(s.fields[1].ty, TypeRef::I32);
        assert_eq!(s.fields[2].name, "active");
        assert_eq!(s.fields[2].ty, TypeRef::Bool);
    }

    #[test]
    fn parse_enum_definitions() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: colors
    functions: []
    enums:
      - name: Color
        variants:
          - name: Red
            value: 0
          - name: Green
            value: 1
          - name: Blue
            value: 2
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let e = &api.modules[0].enums[0];
        assert_eq!(e.name, "Color");
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Red");
        assert_eq!(e.variants[0].value, 0);
        assert_eq!(e.variants[1].name, "Green");
        assert_eq!(e.variants[1].value, 1);
        assert_eq!(e.variants[2].name, "Blue");
        assert_eq!(e.variants[2].value, 2);
    }

    #[test]
    fn parse_optional_types() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: ops
    functions:
      - name: maybe
        params:
          - name: label
            type: "string?"
          - name: count
            type: "i32?"
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let params = &api.modules[0].functions[0].params;
        assert_eq!(
            params[0].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(params[1].ty, TypeRef::Optional(Box::new(TypeRef::I32)));
    }

    #[test]
    fn parse_list_types() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: ops
    functions:
      - name: batch
        params:
          - name: ids
            type: "[i32]"
          - name: names
            type: "[string]"
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let params = &api.modules[0].functions[0].params;
        assert_eq!(params[0].ty, TypeRef::List(Box::new(TypeRef::I32)));
        assert_eq!(params[1].ty, TypeRef::List(Box::new(TypeRef::StringUtf8)));
    }

    #[test]
    fn parse_struct_ref_in_function() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions:
      - name: save
        params:
          - name: contact
            type: Contact
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let param = &api.modules[0].functions[0].params[0];
        assert_eq!(param.name, "contact");
        assert_eq!(param.ty, TypeRef::Struct("Contact".to_string()));
    }

    #[test]
    fn parse_complex_nested_types() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: ops
    functions:
      - name: complex
        params:
          - name: opt_contacts
            type: "[Contact?]"
          - name: maybe_ids
            type: "[i32]?"
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let params = &api.modules[0].functions[0].params;
        assert_eq!(
            params[0].ty,
            TypeRef::List(Box::new(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".to_string()
            )))))
        );
        assert_eq!(
            params[1].ty,
            TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::I32))))
        );
    }
}
