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
    fn parse_contacts_sample() {
        let yaml = std::fs::read_to_string("../../samples/contacts/contacts.yml").unwrap();
        let api = parse_api_str(&yaml, "yml").unwrap();
        assert_eq!(api.version, "0.1.0");
        assert_eq!(api.modules.len(), 1);

        let m = &api.modules[0];
        assert_eq!(m.name, "contacts");

        assert_eq!(m.enums.len(), 1);
        let e = &m.enums[0];
        assert_eq!(e.name, "ContactType");
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Personal");
        assert_eq!(e.variants[0].value, 0);
        assert_eq!(e.variants[1].name, "Work");
        assert_eq!(e.variants[1].value, 1);
        assert_eq!(e.variants[2].name, "Other");
        assert_eq!(e.variants[2].value, 2);

        assert_eq!(m.structs.len(), 1);
        let s = &m.structs[0];
        assert_eq!(s.name, "Contact");
        assert_eq!(s.fields.len(), 5);
        assert_eq!(s.fields[0].ty, TypeRef::I64);
        assert_eq!(
            s.fields[3].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(s.fields[4].ty, TypeRef::Struct("ContactType".to_string()));

        assert_eq!(m.functions.len(), 5);
        assert_eq!(m.functions[0].name, "create_contact");
        assert_eq!(m.functions[0].returns, Some(TypeRef::Handle));
        assert_eq!(m.functions[1].name, "get_contact");
        assert_eq!(
            m.functions[1].returns,
            Some(TypeRef::Struct("Contact".to_string()))
        );
        assert_eq!(m.functions[2].name, "list_contacts");
        assert_eq!(
            m.functions[2].returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct(
                "Contact".to_string()
            ))))
        );
        assert_eq!(m.functions[3].name, "delete_contact");
        assert_eq!(m.functions[3].returns, Some(TypeRef::Bool));
        assert_eq!(m.functions[4].name, "count_contacts");
        assert_eq!(m.functions[4].returns, Some(TypeRef::I32));
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

    #[test]
    fn doc_example_primitives() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: demo
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
      - name: scale
        params:
          - { name: value, type: f64 }
          - { name: factor, type: f64 }
        return: f64
      - name: count
        params:
          - { name: limit, type: u32 }
        return: u32
      - name: timestamp
        params: []
        return: i64
      - name: is_valid
        params:
          - { name: token, type: string }
        return: bool
      - name: echo
        params:
          - { name: message, type: string }
        return: string
      - name: compress
        params:
          - { name: data, type: bytes }
        return: bytes
      - name: open_resource
        params:
          - { name: path, type: string }
        return: handle
      - name: close_resource
        params:
          - { name: id, type: handle }
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let fns = &api.modules[0].functions;
        assert_eq!(fns.len(), 9);
        assert_eq!(fns[0].params[0].ty, TypeRef::I32);
        assert_eq!(fns[0].returns, Some(TypeRef::I32));
        assert_eq!(fns[1].params[0].ty, TypeRef::F64);
        assert_eq!(fns[1].returns, Some(TypeRef::F64));
        assert_eq!(fns[2].params[0].ty, TypeRef::U32);
        assert_eq!(fns[2].returns, Some(TypeRef::U32));
        assert_eq!(fns[3].returns, Some(TypeRef::I64));
        assert_eq!(fns[4].params[0].ty, TypeRef::StringUtf8);
        assert_eq!(fns[4].returns, Some(TypeRef::Bool));
        assert_eq!(fns[5].returns, Some(TypeRef::StringUtf8));
        assert_eq!(fns[6].params[0].ty, TypeRef::Bytes);
        assert_eq!(fns[6].returns, Some(TypeRef::Bytes));
        assert_eq!(fns[7].returns, Some(TypeRef::Handle));
        assert_eq!(fns[8].params[0].ty, TypeRef::Handle);
        assert_eq!(fns[8].returns, None);
    }

    #[test]
    fn doc_example_structs() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: geometry
    structs:
      - name: Point
        doc: "A 2D point in space"
        fields:
          - name: x
            type: f64
            doc: "X coordinate"
          - name: "y"
            type: f64
            doc: "Y coordinate"
      - name: Rect
        fields:
          - name: origin
            type: Point
          - name: width
            type: f64
          - name: height
            type: f64
    functions:
      - name: distance
        params:
          - { name: a, type: Point }
          - { name: b, type: Point }
        return: f64
      - name: bounding_box
        params:
          - { name: points, type: "[Point]" }
        return: Rect
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let m = &api.modules[0];
        assert_eq!(m.structs.len(), 2);
        assert_eq!(m.structs[0].name, "Point");
        assert_eq!(m.structs[0].doc.as_deref(), Some("A 2D point in space"));
        assert_eq!(m.structs[0].fields[0].doc.as_deref(), Some("X coordinate"));
        assert_eq!(m.structs[1].name, "Rect");
        assert_eq!(
            m.structs[1].fields[0].ty,
            TypeRef::Struct("Point".to_string())
        );

        assert_eq!(m.functions[0].params[0].ty, TypeRef::Struct("Point".into()));
        assert_eq!(m.functions[0].returns, Some(TypeRef::F64));
        assert_eq!(
            m.functions[1].params[0].ty,
            TypeRef::List(Box::new(TypeRef::Struct("Point".into())))
        );
        assert_eq!(m.functions[1].returns, Some(TypeRef::Struct("Rect".into())));
    }

    #[test]
    fn doc_example_enums() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        doc: "Category of contact"
        variants:
          - name: Personal
            value: 0
            doc: "Friends and family"
          - name: Work
            value: 1
            doc: "Professional contacts"
          - name: Other
            value: 2
    functions:
      - name: count_by_type
        params:
          - { name: contact_type, type: ContactType }
        return: i32
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let m = &api.modules[0];
        assert_eq!(m.enums.len(), 1);
        let e = &m.enums[0];
        assert_eq!(e.name, "ContactType");
        assert_eq!(e.doc.as_deref(), Some("Category of contact"));
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].doc.as_deref(), Some("Friends and family"));
        assert_eq!(e.variants[2].doc, None);
        assert_eq!(
            m.functions[0].params[0].ty,
            TypeRef::Struct("ContactType".into())
        );
    }

    #[test]
    fn doc_example_optionals() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    structs:
      - name: Contact
        fields:
          - { name: id, type: i64 }
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: nickname, type: "string?" }
    functions:
      - name: find_contact
        params:
          - { name: id, type: i64 }
        return: "Contact?"
        doc: "Returns null if no contact exists with the given id"
      - name: update_email
        params:
          - { name: id, type: i64 }
          - { name: email, type: "string?" }
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let m = &api.modules[0];
        let s = &m.structs[0];
        assert_eq!(
            s.fields[2].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(
            m.functions[0].returns,
            Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
        assert_eq!(
            m.functions[1].params[1].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
    }

    #[test]
    fn doc_example_lists() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: ops
    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
    functions:
      - name: sum
        params:
          - { name: values, type: "[i32]" }
        return: i32
      - name: list_contacts
        params: []
        return: "[Contact]"
      - name: batch_delete
        params:
          - { name: ids, type: "[i64]" }
        return: i32
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let fns = &api.modules[0].functions;
        assert_eq!(fns[0].params[0].ty, TypeRef::List(Box::new(TypeRef::I32)));
        assert_eq!(
            fns[1].returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))))
        );
        assert_eq!(fns[2].params[0].ty, TypeRef::List(Box::new(TypeRef::I64)));
    }

    #[test]
    fn doc_example_nested_types() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: ops
    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
    functions:
      - name: search
        params:
          - { name: query, type: string }
        return: "[Contact?]"
      - name: get_scores
        params:
          - { name: user_id, type: i64 }
        return: "[i32]?"
      - name: bulk_update
        params:
          - { name: emails, type: "[string?]" }
        return: i32
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let fns = &api.modules[0].functions;
        assert_eq!(
            fns[0].returns,
            Some(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                TypeRef::Struct("Contact".into())
            )))))
        );
        assert_eq!(
            fns[1].returns,
            Some(TypeRef::Optional(Box::new(TypeRef::List(Box::new(
                TypeRef::I32
            )))))
        );
        assert_eq!(
            fns[2].params[0].ty,
            TypeRef::List(Box::new(TypeRef::Optional(Box::new(TypeRef::StringUtf8))))
        );
    }

    #[test]
    fn doc_example_complete_contacts() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }
    structs:
      - name: Contact
        doc: "A contact record"
        fields:
          - { name: id, type: i64 }
          - { name: first_name, type: string }
          - { name: last_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
    functions:
      - name: create_contact
        params:
          - { name: first_name, type: string }
          - { name: last_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
        return: handle
      - name: get_contact
        params:
          - { name: id, type: handle }
        return: Contact
      - name: list_contacts
        params: []
        return: "[Contact]"
      - name: delete_contact
        params:
          - { name: id, type: handle }
        return: bool
      - name: count_contacts
        params: []
        return: i32
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        assert_eq!(api.version, "0.1.0");
        let m = &api.modules[0];
        assert_eq!(m.name, "contacts");
        assert_eq!(m.enums.len(), 1);
        assert_eq!(m.enums[0].variants.len(), 3);
        assert_eq!(m.structs.len(), 1);
        assert_eq!(m.structs[0].fields.len(), 5);
        assert_eq!(m.functions.len(), 5);
        assert_eq!(m.functions[0].returns, Some(TypeRef::Handle));
        assert_eq!(
            m.functions[1].returns,
            Some(TypeRef::Struct("Contact".into()))
        );
        assert_eq!(
            m.functions[2].returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))))
        );
        assert_eq!(m.functions[3].returns, Some(TypeRef::Bool));
        assert_eq!(m.functions[4].returns, Some(TypeRef::I32));
    }
}
