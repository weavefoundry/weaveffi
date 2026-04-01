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
                            mutable: false,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: Some("Adds two numbers".to_string()),
                    r#async: false,
                    cancellable: false,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
    fn doc_example_error_domain() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    errors:
      name: ContactErrors
      codes:
        - name: not_found
          code: 1
          message: "Contact not found"
        - name: duplicate
          code: 2
          message: "Contact already exists"
        - name: invalid_email
          code: 3
          message: "Email address is invalid"
    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: email, type: string }
        return: handle
      - name: get_contact
        params:
          - { name: id, type: handle }
        return: string
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let m = &api.modules[0];
        assert_eq!(m.name, "contacts");

        let errors = m.errors.as_ref().expect("error domain should be present");
        assert_eq!(errors.name, "ContactErrors");
        assert_eq!(errors.codes.len(), 3);
        assert_eq!(errors.codes[0].name, "not_found");
        assert_eq!(errors.codes[0].code, 1);
        assert_eq!(errors.codes[0].message, "Contact not found");
        assert_eq!(errors.codes[1].name, "duplicate");
        assert_eq!(errors.codes[1].code, 2);
        assert_eq!(errors.codes[1].message, "Contact already exists");
        assert_eq!(errors.codes[2].name, "invalid_email");
        assert_eq!(errors.codes[2].code, 3);
        assert_eq!(errors.codes[2].message, "Email address is invalid");

        assert_eq!(m.functions.len(), 2);
        assert_eq!(m.functions[0].name, "create_contact");
        assert_eq!(m.functions[0].returns, Some(TypeRef::Handle));
        assert_eq!(m.functions[1].name, "get_contact");
        assert_eq!(m.functions[1].returns, Some(TypeRef::StringUtf8));
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

    #[test]
    fn parse_function_with_map_param() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: store
    functions:
      - name: put_scores
        params:
          - name: scores
            type: "{string:i32}"
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let param = &api.modules[0].functions[0].params[0];
        assert_eq!(param.name, "scores");
        assert_eq!(
            param.ty,
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32))
        );
    }

    #[test]
    fn parse_function_with_map_return() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: store
    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
    functions:
      - name: get_contacts
        params: []
        return: "{string:Contact}"
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.name, "get_contacts");
        assert_eq!(
            f.returns,
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::Struct("Contact".into()))
            ))
        );
    }

    #[test]
    fn parse_inventory_sample() {
        let yaml = std::fs::read_to_string("../../samples/inventory/inventory.yml").unwrap();
        let api = parse_api_str(&yaml, "yml").unwrap();
        assert_eq!(api.version, "0.1.0");
        assert_eq!(api.modules.len(), 2);

        let products = &api.modules[0];
        assert_eq!(products.name, "products");
        assert_eq!(products.enums.len(), 1);
        assert_eq!(products.structs.len(), 1);
        assert_eq!(products.functions.len(), 5);

        let cat = &products.enums[0];
        assert_eq!(cat.name, "Category");
        assert_eq!(cat.variants.len(), 4);
        assert_eq!(cat.variants[0].name, "Electronics");
        assert_eq!(cat.variants[0].value, 0);
        assert_eq!(cat.variants[3].name, "Books");
        assert_eq!(cat.variants[3].value, 3);

        let product = &products.structs[0];
        assert_eq!(product.name, "Product");
        assert_eq!(product.fields.len(), 6);
        assert_eq!(product.fields[0].ty, TypeRef::I64);
        assert_eq!(product.fields[1].ty, TypeRef::StringUtf8);
        assert_eq!(
            product.fields[2].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(product.fields[3].ty, TypeRef::F64);
        assert_eq!(
            product.fields[4].ty,
            TypeRef::Struct("Category".to_string())
        );
        assert_eq!(
            product.fields[5].ty,
            TypeRef::List(Box::new(TypeRef::StringUtf8))
        );

        assert_eq!(products.functions[0].name, "create_product");
        assert_eq!(products.functions[0].returns, Some(TypeRef::Handle));
        assert_eq!(products.functions[1].name, "get_product");
        assert_eq!(
            products.functions[1].returns,
            Some(TypeRef::Struct("Product".to_string()))
        );
        assert_eq!(products.functions[2].name, "search_products");
        assert_eq!(
            products.functions[2].returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct(
                "Product".to_string()
            ))))
        );
        assert_eq!(products.functions[3].name, "update_price");
        assert_eq!(products.functions[3].returns, Some(TypeRef::Bool));
        assert_eq!(products.functions[4].name, "delete_product");
        assert_eq!(products.functions[4].returns, Some(TypeRef::Bool));

        let orders = &api.modules[1];
        assert_eq!(orders.name, "orders");
        assert_eq!(orders.enums.len(), 0);
        assert_eq!(orders.structs.len(), 2);
        assert_eq!(orders.functions.len(), 4);

        let order_item = &orders.structs[0];
        assert_eq!(order_item.name, "OrderItem");
        assert_eq!(order_item.fields.len(), 3);

        let order = &orders.structs[1];
        assert_eq!(order.name, "Order");
        assert_eq!(order.fields.len(), 4);
        assert_eq!(
            order.fields[1].ty,
            TypeRef::List(Box::new(TypeRef::Struct("OrderItem".to_string())))
        );

        assert_eq!(orders.functions[0].name, "create_order");
        assert_eq!(orders.functions[0].returns, Some(TypeRef::Handle));
        assert_eq!(orders.functions[1].name, "get_order");
        assert_eq!(
            orders.functions[1].returns,
            Some(TypeRef::Struct("Order".to_string()))
        );
        assert_eq!(orders.functions[2].name, "cancel_order");
        assert_eq!(orders.functions[2].returns, Some(TypeRef::Bool));
        assert_eq!(orders.functions[3].name, "add_product_to_order");
        assert_eq!(orders.functions[3].returns, Some(TypeRef::Bool));
        assert_eq!(orders.functions[3].params.len(), 2);
        assert_eq!(orders.functions[3].params[0].name, "order_id");
        assert_eq!(orders.functions[3].params[0].ty, TypeRef::Handle);
        assert_eq!(orders.functions[3].params[1].name, "product");
        assert_eq!(
            orders.functions[3].params[1].ty,
            TypeRef::Struct("Product".to_string())
        );
    }

    #[test]
    fn parse_async_demo_sample() {
        let yaml = std::fs::read_to_string("../../samples/async-demo/async_demo.yml").unwrap();
        let api = parse_api_str(&yaml, "yml").unwrap();
        assert_eq!(api.version, "0.2.0");
        assert_eq!(api.modules.len(), 1);

        let m = &api.modules[0];
        assert_eq!(m.name, "tasks");

        assert_eq!(m.structs.len(), 1);
        let s = &m.structs[0];
        assert_eq!(s.name, "TaskResult");
        assert_eq!(s.fields.len(), 3);
        assert_eq!(s.fields[0].name, "id");
        assert_eq!(s.fields[0].ty, TypeRef::I64);
        assert_eq!(s.fields[1].name, "value");
        assert_eq!(s.fields[1].ty, TypeRef::StringUtf8);
        assert_eq!(s.fields[2].name, "success");
        assert_eq!(s.fields[2].ty, TypeRef::Bool);

        assert_eq!(m.functions.len(), 3);

        let run_task = &m.functions[0];
        assert_eq!(run_task.name, "run_task");
        assert!(run_task.r#async);
        assert_eq!(
            run_task.returns,
            Some(TypeRef::Struct("TaskResult".to_string()))
        );

        let run_batch = &m.functions[1];
        assert_eq!(run_batch.name, "run_batch");
        assert!(run_batch.r#async);
        assert_eq!(
            run_batch.returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct(
                "TaskResult".to_string()
            ))))
        );

        let cancel_task = &m.functions[2];
        assert_eq!(cancel_task.name, "cancel_task");
        assert!(!cancel_task.r#async);
        assert_eq!(cancel_task.returns, Some(TypeRef::Bool));
    }

    #[test]
    fn parse_idl_doc_map_example() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: example
    structs:
      - name: Contact
        fields:
          - { name: id, type: i64 }
          - { name: name, type: string }
          - { name: email, type: "string?" }
    functions:
      - name: update_scores
        params:
          - { name: scores, type: "{string:i32}" }
        return: bool

      - name: get_contacts
        params: []
        return: "{string:Contact}"

      - name: merge_tags
        params:
          - { name: current, type: "{string:string}" }
          - { name: additions, type: "{string:string}" }
        return: "{string:string}"
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        let m = &api.modules[0];
        assert_eq!(m.functions.len(), 3);

        let f0 = &m.functions[0];
        assert_eq!(f0.name, "update_scores");
        assert_eq!(
            f0.params[0].ty,
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32))
        );
        assert_eq!(f0.returns, Some(TypeRef::Bool));

        let f1 = &m.functions[1];
        assert_eq!(f1.name, "get_contacts");
        assert_eq!(
            f1.returns,
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::Struct("Contact".into()))
            ))
        );

        let f2 = &m.functions[2];
        assert_eq!(f2.name, "merge_tags");
        assert_eq!(
            f2.params[0].ty,
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(
            f2.returns,
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::StringUtf8)
            ))
        );
    }

    #[test]
    fn parse_nested_modules() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: parent
    functions:
      - name: top_fn
        params: []
        return: i32
    modules:
      - name: child
        functions:
          - name: inner_fn
            params:
              - name: x
                type: i32
            return: i32
"#;
        let api = parse_api_str(yaml, "yaml").unwrap();
        assert_eq!(api.modules.len(), 1);
        let parent = &api.modules[0];
        assert_eq!(parent.name, "parent");
        assert_eq!(parent.functions.len(), 1);
        assert_eq!(parent.functions[0].name, "top_fn");

        assert_eq!(parent.modules.len(), 1);
        let child = &parent.modules[0];
        assert_eq!(child.name, "child");
        assert_eq!(child.functions.len(), 1);
        assert_eq!(child.functions[0].name, "inner_fn");
        assert_eq!(child.functions[0].params[0].name, "x");
        assert_eq!(child.functions[0].params[0].ty, TypeRef::I32);
        assert_eq!(child.functions[0].returns, Some(TypeRef::I32));
    }
}
