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
}
