use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: &str = "0.2.0";

/// `Eq` is omitted because `toml::Value` contains `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Api {
    pub version: String,
    pub modules: Vec<Module>,
    #[serde(default)]
    pub generators: Option<HashMap<String, toml::Value>>,
}

/// `Eq` is omitted because `StructField::default` contains `serde_yaml::Value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Module {
    pub name: String,
    pub functions: Vec<Function>,
    #[serde(default)]
    pub structs: Vec<StructDef>,
    #[serde(default)]
    pub enums: Vec<EnumDef>,
    #[serde(default)]
    pub callbacks: Vec<CallbackDef>,
    #[serde(default)]
    pub listeners: Vec<ListenerDef>,
    #[serde(default)]
    pub errors: Option<ErrorDomain>,
    #[serde(default)]
    pub modules: Vec<Module>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    #[serde(rename = "return", default)]
    pub returns: Option<TypeRef>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default, rename = "async")]
    pub r#async: bool,
    #[serde(default)]
    pub cancellable: bool,
    #[serde(default)]
    pub deprecated: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub mutable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackDef {
    pub name: String,
    pub params: Vec<Param>,
    #[serde(default)]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerDef {
    pub name: String,
    pub event_callback: String,
    #[serde(default)]
    pub doc: Option<String>,
}

/// A reference to a type in the IDL.
///
/// Callback-style behavior is **not** expressed as a `TypeRef` variant.
/// Instead, callbacks and listeners are declared at the module level via
/// `Module.callbacks` (see [`CallbackDef`]) and `Module.listeners` (see
/// [`ListenerDef`]), and asynchronous functions use `async: true`. These
/// primitives cover every pattern the FFI boundary needs to support, and
/// keep the type system free of function-typed values that the C ABI
/// cannot represent uniformly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    I32,
    U32,
    I64,
    F64,
    Bool,
    StringUtf8,
    Bytes,
    Handle,
    TypedHandle(String),
    Struct(String),
    Enum(String),
    BorrowedStr,
    BorrowedBytes,
    Optional(Box<TypeRef>),
    List(Box<TypeRef>),
    Map(Box<TypeRef>, Box<TypeRef>),
    Iterator(Box<TypeRef>),
}

pub fn parse_type_ref(s: &str) -> Result<TypeRef, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty type reference".to_string());
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        return parse_type_ref(inner).map(|t| TypeRef::List(Box::new(t)));
    }
    if s.starts_with('{') && s.ends_with('}') {
        let inner = &s[1..s.len() - 1];
        let colon = inner
            .find(':')
            .ok_or_else(|| "map type missing ':' separator".to_string())?;
        let key = parse_type_ref(&inner[..colon])?;
        let val = parse_type_ref(&inner[colon + 1..])?;
        return Ok(TypeRef::Map(Box::new(key), Box::new(val)));
    }
    if let Some(inner) = s.strip_suffix('?') {
        return parse_type_ref(inner).map(|t| TypeRef::Optional(Box::new(t)));
    }
    if let Some(inner) = s
        .strip_prefix("handle<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return Ok(TypeRef::TypedHandle(inner.into()));
    }
    if let Some(inner) = s
        .strip_prefix("iter<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return parse_type_ref(inner).map(|t| TypeRef::Iterator(Box::new(t)));
    }
    match s {
        "i32" => Ok(TypeRef::I32),
        "u32" => Ok(TypeRef::U32),
        "i64" => Ok(TypeRef::I64),
        "f64" => Ok(TypeRef::F64),
        "bool" => Ok(TypeRef::Bool),
        "string" => Ok(TypeRef::StringUtf8),
        "bytes" => Ok(TypeRef::Bytes),
        "handle" => Ok(TypeRef::Handle),
        "&str" => Ok(TypeRef::BorrowedStr),
        "&[u8]" => Ok(TypeRef::BorrowedBytes),
        name => Ok(TypeRef::Struct(name.to_string())),
    }
}

fn type_ref_to_string(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "i32".to_string(),
        TypeRef::U32 => "u32".to_string(),
        TypeRef::I64 => "i64".to_string(),
        TypeRef::F64 => "f64".to_string(),
        TypeRef::Bool => "bool".to_string(),
        TypeRef::StringUtf8 => "string".to_string(),
        TypeRef::Bytes => "bytes".to_string(),
        TypeRef::BorrowedStr => "&str".to_string(),
        TypeRef::BorrowedBytes => "&[u8]".to_string(),
        TypeRef::Handle => "handle".to_string(),
        TypeRef::TypedHandle(name) => format!("handle<{name}>"),
        TypeRef::Struct(name) | TypeRef::Enum(name) => name.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_ref_to_string(inner)),
        TypeRef::List(inner) => format!("[{}]", type_ref_to_string(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_ref_to_string(k), type_ref_to_string(v)),
        TypeRef::Iterator(inner) => format!("iter<{}>", type_ref_to_string(inner)),
    }
}

impl Serialize for TypeRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&type_ref_to_string(self))
    }
}

impl<'de> Deserialize<'de> for TypeRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_type_ref(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumDef {
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    pub variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumVariant {
    pub name: String,
    pub value: i32,
    #[serde(default)]
    pub doc: Option<String>,
}

/// `Eq` is omitted because `StructField::default` contains `serde_yaml::Value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructDef {
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    pub fields: Vec<StructField>,
    #[serde(default)]
    pub builder: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDomain {
    pub name: String,
    pub codes: Vec<ErrorCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorCode {
    pub name: String,
    pub code: i32,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_def_round_trip_yaml() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: geometry
    functions: []
    structs:
      - name: Point
        doc: "A 2D point"
        fields:
          - name: x
            type: f64
          - name: "y"
            type: f64
            doc: "Y coordinate"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.structs.len(), 1);
        let s = &m.structs[0];
        assert_eq!(s.name, "Point");
        assert_eq!(s.doc.as_deref(), Some("A 2D point"));
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.fields[0].name, "x");
        assert_eq!(s.fields[0].ty, TypeRef::F64);
        assert_eq!(s.fields[0].doc, None);
        assert_eq!(s.fields[1].name, "y");
        assert_eq!(s.fields[1].doc.as_deref(), Some("Y coordinate"));
    }

    #[test]
    fn struct_def_round_trip_json() {
        let json = r#"{
            "version": "0.1.0",
            "modules": [{
                "name": "geo",
                "functions": [],
                "structs": [{
                    "name": "Rect",
                    "fields": [
                        {"name": "width", "type": "i32"},
                        {"name": "height", "type": "i32"}
                    ]
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        let s = &api.modules[0].structs[0];
        assert_eq!(s.name, "Rect");
        assert_eq!(s.doc, None);
        assert_eq!(s.fields[0].ty, TypeRef::I32);
    }

    #[test]
    fn structs_default_to_empty() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].structs.is_empty());
    }

    #[test]
    fn typeref_struct_variant_serializes() {
        let ty = TypeRef::Struct("Point".to_string());
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""Point""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn struct_field_with_struct_type() {
        let field = StructField {
            name: "origin".to_string(),
            ty: TypeRef::Struct("Point".to_string()),
            doc: None,
            default: None,
        };
        let json = serde_json::to_string(&field).unwrap();
        let back: StructField = serde_json::from_str(&json).unwrap();
        assert_eq!(back, field);
    }

    #[test]
    fn typeref_is_not_copy() {
        let a = TypeRef::Struct("Foo".to_string());
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn enum_def_round_trip_yaml() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: graphics
    functions: []
    enums:
      - name: Color
        doc: "Primary colors"
        variants:
          - name: Red
            value: 0
          - name: Green
            value: 1
            doc: "The color green"
          - name: Blue
            value: 2
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.enums.len(), 1);
        let e = &m.enums[0];
        assert_eq!(e.name, "Color");
        assert_eq!(e.doc.as_deref(), Some("Primary colors"));
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Red");
        assert_eq!(e.variants[0].value, 0);
        assert_eq!(e.variants[0].doc, None);
        assert_eq!(e.variants[1].name, "Green");
        assert_eq!(e.variants[1].value, 1);
        assert_eq!(e.variants[1].doc.as_deref(), Some("The color green"));
        assert_eq!(e.variants[2].name, "Blue");
        assert_eq!(e.variants[2].value, 2);
    }

    #[test]
    fn enum_def_round_trip_json() {
        let json = r#"{
            "version": "0.1.0",
            "modules": [{
                "name": "status",
                "functions": [],
                "enums": [{
                    "name": "Status",
                    "variants": [
                        {"name": "Ok", "value": 0},
                        {"name": "Error", "value": 1}
                    ]
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        let e = &api.modules[0].enums[0];
        assert_eq!(e.name, "Status");
        assert_eq!(e.doc, None);
        assert_eq!(e.variants.len(), 2);
        assert_eq!(e.variants[1].value, 1);
    }

    #[test]
    fn enums_default_to_empty() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].enums.is_empty());
    }

    #[test]
    fn typeref_enum_variant_serializes_as_name() {
        let ty = TypeRef::Enum("Color".to_string());
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""Color""#);
    }

    #[test]
    fn enum_def_clone_and_eq() {
        let e = EnumDef {
            name: "Direction".to_string(),
            doc: Some("Cardinal directions".to_string()),
            variants: vec![
                EnumVariant {
                    name: "North".to_string(),
                    value: 0,
                    doc: None,
                },
                EnumVariant {
                    name: "South".to_string(),
                    value: 1,
                    doc: None,
                },
            ],
        };
        assert_eq!(e, e.clone());
    }

    #[test]
    fn struct_def_clone_and_eq() {
        let s = StructDef {
            name: "Color".to_string(),
            doc: Some("RGB color".to_string()),
            fields: vec![
                StructField {
                    name: "r".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "g".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "b".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
            ],
            builder: false,
        };
        assert_eq!(s, s.clone());
    }

    #[test]
    fn parse_type_ref_primitives() {
        assert_eq!(parse_type_ref("i32"), Ok(TypeRef::I32));
        assert_eq!(parse_type_ref("u32"), Ok(TypeRef::U32));
        assert_eq!(parse_type_ref("i64"), Ok(TypeRef::I64));
        assert_eq!(parse_type_ref("f64"), Ok(TypeRef::F64));
        assert_eq!(parse_type_ref("bool"), Ok(TypeRef::Bool));
        assert_eq!(parse_type_ref("string"), Ok(TypeRef::StringUtf8));
        assert_eq!(parse_type_ref("bytes"), Ok(TypeRef::Bytes));
        assert_eq!(parse_type_ref("handle"), Ok(TypeRef::Handle));
    }

    #[test]
    fn parse_type_ref_struct() {
        assert_eq!(
            parse_type_ref("Contact"),
            Ok(TypeRef::Struct("Contact".into()))
        );
        assert_eq!(
            parse_type_ref("MyWidget"),
            Ok(TypeRef::Struct("MyWidget".into()))
        );
    }

    #[test]
    fn parse_type_ref_optional() {
        assert_eq!(
            parse_type_ref("string?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::StringUtf8)))
        );
        assert_eq!(
            parse_type_ref("i32?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            parse_type_ref("Contact?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
    }

    #[test]
    fn parse_type_ref_list() {
        assert_eq!(
            parse_type_ref("[i32]"),
            Ok(TypeRef::List(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            parse_type_ref("[string]"),
            Ok(TypeRef::List(Box::new(TypeRef::StringUtf8)))
        );
        assert_eq!(
            parse_type_ref("[Contact]"),
            Ok(TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))))
        );
    }

    #[test]
    fn parse_type_ref_nested() {
        assert_eq!(
            parse_type_ref("[i32?]"),
            Ok(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                TypeRef::I32
            )))))
        );
        assert_eq!(
            parse_type_ref("[Contact]?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::List(Box::new(
                TypeRef::Struct("Contact".into())
            )))))
        );
    }

    #[test]
    fn parse_type_ref_empty_is_error() {
        assert!(parse_type_ref("").is_err());
        assert!(parse_type_ref("  ").is_err());
    }

    #[test]
    fn typeref_primitive_round_trips() {
        for ty in [
            TypeRef::I32,
            TypeRef::U32,
            TypeRef::I64,
            TypeRef::F64,
            TypeRef::Bool,
            TypeRef::StringUtf8,
            TypeRef::Bytes,
            TypeRef::Handle,
        ] {
            let json = serde_json::to_string(&ty).unwrap();
            let back: TypeRef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ty);
        }
    }

    #[test]
    fn typeref_optional_round_trip() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""string?""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_list_round_trip() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""[i32]""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_optional_struct_round_trip() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into())));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""Contact?""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_list_struct_round_trip() {
        let ty = TypeRef::List(Box::new(TypeRef::Struct("Contact".into())));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""[Contact]""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_optional_yaml_deser() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions:
      - name: find
        params:
          - name: id
            type: i32
        return: "Contact?"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.returns,
            Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
    }

    #[test]
    fn typeref_list_yaml_deser() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions:
      - name: list_all
        params: []
        return: "[Contact]"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))))
        );
    }

    #[test]
    fn typeref_hash_works_with_box_variants() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(TypeRef::I32);
        set.insert(TypeRef::Optional(Box::new(TypeRef::I32)));
        set.insert(TypeRef::List(Box::new(TypeRef::I32)));
        set.insert(TypeRef::Optional(Box::new(TypeRef::Struct("Foo".into()))));
        set.insert(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32),
        ));
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn parse_type_ref_map_primitives() {
        assert_eq!(
            parse_type_ref("{string:i32}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            ))
        );
    }

    #[test]
    fn parse_type_ref_map_struct_value() {
        assert_eq!(
            parse_type_ref("{string:Contact}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::Struct("Contact".into()))
            ))
        );
    }

    #[test]
    fn parse_type_ref_map_nested_value() {
        assert_eq!(
            parse_type_ref("{string:[i32]}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::List(Box::new(TypeRef::I32)))
            ))
        );
    }

    #[test]
    fn parse_type_ref_map_missing_colon() {
        assert!(parse_type_ref("{string}").is_err());
    }

    #[test]
    fn typeref_map_round_trip() {
        let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""{string:i32}""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_map_struct_round_trip() {
        let ty = TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::Struct("Contact".into())),
        );
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""{string:Contact}""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_map_yaml_deser() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions:
      - name: get_metadata
        params: []
        return: "{string:i32}"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.returns,
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            ))
        );
    }

    #[test]
    fn typeref_optional_map_round_trip() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32),
        )));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""{string:i32}?""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn parse_map_string_to_i32() {
        assert_eq!(
            parse_type_ref("{string:i32}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            ))
        );
    }

    #[test]
    fn parse_map_string_to_struct() {
        assert_eq!(
            parse_type_ref("{string:Contact}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::Struct("Contact".into())),
            ))
        );
    }

    #[test]
    fn parse_map_roundtrip() {
        let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn parse_optional_map() {
        assert_eq!(
            parse_type_ref("{string:i32}?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            ))))
        );
    }

    #[test]
    fn parse_map_of_lists() {
        assert_eq!(
            parse_type_ref("{string:[i32]}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::List(Box::new(TypeRef::I32))),
            ))
        );
    }

    #[test]
    fn parse_type_ref_iterator() {
        assert_eq!(
            parse_type_ref("iter<i32>"),
            Ok(TypeRef::Iterator(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            parse_type_ref("iter<string>"),
            Ok(TypeRef::Iterator(Box::new(TypeRef::StringUtf8)))
        );
        assert_eq!(
            parse_type_ref("iter<Contact>"),
            Ok(TypeRef::Iterator(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
    }

    #[test]
    fn typeref_iterator_round_trip() {
        let ty = TypeRef::Iterator(Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""iter<i32>""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_iterator_struct_round_trip() {
        let ty = TypeRef::Iterator(Box::new(TypeRef::Struct("Contact".into())));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""iter<Contact>""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn parse_type_ref_borrowed() {
        assert_eq!(parse_type_ref("&str"), Ok(TypeRef::BorrowedStr));
        assert_eq!(parse_type_ref("&[u8]"), Ok(TypeRef::BorrowedBytes));
    }

    #[test]
    fn typeref_borrowed_round_trip() {
        for ty in [TypeRef::BorrowedStr, TypeRef::BorrowedBytes] {
            let json = serde_json::to_string(&ty).unwrap();
            let back: TypeRef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ty);
        }
    }

    #[test]
    fn typeref_borrowed_str_serializes_as_ampersand_str() {
        let json = serde_json::to_string(&TypeRef::BorrowedStr).unwrap();
        assert_eq!(json, r#""&str""#);
    }

    #[test]
    fn typeref_borrowed_bytes_serializes_as_ampersand_u8() {
        let json = serde_json::to_string(&TypeRef::BorrowedBytes).unwrap();
        assert_eq!(json, r#""&[u8]""#);
    }

    #[test]
    fn typeref_borrowed_yaml_deser() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: io
    functions:
      - name: write
        params:
          - name: data
            type: "&str"
          - name: raw
            type: "&[u8]"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::BorrowedStr);
        assert_eq!(f.params[1].ty, TypeRef::BorrowedBytes);
    }

    #[test]
    fn parse_typed_handle() {
        assert_eq!(
            parse_type_ref("handle<Session>"),
            Ok(TypeRef::TypedHandle("Session".into()))
        );
        assert_eq!(parse_type_ref("handle"), Ok(TypeRef::Handle));
    }

    #[test]
    fn generators_field_parses_from_yaml() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions: []
generators:
  swift:
    module_name: MySwiftModule
  android:
    package: com.example.app
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let generators = api.generators.as_ref().unwrap();
        let swift = generators["swift"].as_table().unwrap();
        assert_eq!(swift["module_name"].as_str(), Some("MySwiftModule"));
        let android = generators["android"].as_table().unwrap();
        assert_eq!(android["package"].as_str(), Some("com.example.app"));
    }

    #[test]
    fn generators_defaults_to_none() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.generators.is_none());
    }

    #[test]
    fn parse_typed_handle_roundtrip() {
        let ty = TypeRef::TypedHandle("Connection".into());
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""handle<Connection>""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn callback_def_round_trip_yaml() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: events
    functions: []
    callbacks:
      - name: on_data
        params:
          - name: payload
            type: string
        doc: "Fired when data arrives"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.callbacks.len(), 1);
        let cb = &m.callbacks[0];
        assert_eq!(cb.name, "on_data");
        assert_eq!(cb.params.len(), 1);
        assert_eq!(cb.params[0].name, "payload");
        assert_eq!(cb.params[0].ty, TypeRef::StringUtf8);
        assert_eq!(cb.doc.as_deref(), Some("Fired when data arrives"));
    }

    #[test]
    fn listener_def_round_trip_yaml() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: events
    functions: []
    callbacks:
      - name: on_data
        params: []
    listeners:
      - name: data_stream
        event_callback: on_data
        doc: "Subscribe to data events"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.listeners.len(), 1);
        let l = &m.listeners[0];
        assert_eq!(l.name, "data_stream");
        assert_eq!(l.event_callback, "on_data");
        assert_eq!(l.doc.as_deref(), Some("Subscribe to data events"));
    }

    #[test]
    fn callbacks_and_listeners_default_to_empty() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].callbacks.is_empty());
        assert!(api.modules[0].listeners.is_empty());
    }

    #[test]
    fn callback_def_json_round_trip() {
        let cb = CallbackDef {
            name: "on_event".to_string(),
            params: vec![Param {
                name: "data".to_string(),
                ty: TypeRef::I32,
                mutable: false,
            }],
            doc: Some("event callback".to_string()),
        };
        let json = serde_json::to_string(&cb).unwrap();
        let back: CallbackDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cb);
    }

    #[test]
    fn listener_def_json_round_trip() {
        let l = ListenerDef {
            name: "watcher".to_string(),
            event_callback: "on_change".to_string(),
            doc: None,
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: ListenerDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn builder_defaults_to_false() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(!api.modules[0].structs[0].builder);
    }

    #[test]
    fn builder_true_round_trip() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
        builder: true
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].structs[0].builder);

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        assert!(back.modules[0].structs[0].builder);
    }

    #[test]
    fn builder_false_explicit() {
        let json = r#"{
            "version": "0.1.0",
            "modules": [{
                "name": "geo",
                "functions": [],
                "structs": [{
                    "name": "Point",
                    "fields": [{"name": "x", "type": "f64"}],
                    "builder": false
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        assert!(!api.modules[0].structs[0].builder);
    }

    #[test]
    fn param_mutable_defaults_to_false() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: io
    functions:
      - name: write
        params:
          - name: data
            type: string
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(!api.modules[0].functions[0].params[0].mutable);
    }

    #[test]
    fn param_mutable_true_round_trip() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: io
    functions:
      - name: fill_buffer
        params:
          - name: buf
            type: bytes
            mutable: true
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].functions[0].params[0].mutable);

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        assert!(back.modules[0].functions[0].params[0].mutable);
    }

    #[test]
    fn param_mutable_false_explicit() {
        let json = r#"{
            "version": "0.1.0",
            "modules": [{
                "name": "io",
                "functions": [{
                    "name": "read",
                    "params": [{"name": "buf", "type": "bytes", "mutable": false}]
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        assert!(!api.modules[0].functions[0].params[0].mutable);
    }

    #[test]
    fn deprecated_and_since_default_to_none() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions:
      - name: add
        params: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.deprecated, None);
        assert_eq!(f.since, None);
    }

    #[test]
    fn deprecated_and_since_round_trip() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: math
    functions:
      - name: add_old
        params: []
        deprecated: "Use add_v2 instead"
        since: "0.1.0"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.deprecated.as_deref(), Some("Use add_v2 instead"));
        assert_eq!(f.since.as_deref(), Some("0.1.0"));

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        let f2 = &back.modules[0].functions[0];
        assert_eq!(f2.deprecated.as_deref(), Some("Use add_v2 instead"));
        assert_eq!(f2.since.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn struct_field_default_value_round_trip() {
        let yaml = r#"
version: "0.1.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
          - name: age
            type: i32
            default: 0
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let fields = &api.modules[0].structs[0].fields;
        assert!(fields[0].default.is_none());
        assert_eq!(
            fields[1].default,
            Some(serde_yaml::Value::Number(serde_yaml::Number::from(0)))
        );
    }

    #[test]
    fn parse_type_ref_does_not_yield_callback() {
        assert_eq!(
            parse_type_ref("callback"),
            Ok(TypeRef::Struct("callback".into()))
        );
    }
}
