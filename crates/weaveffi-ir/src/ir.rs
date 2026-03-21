use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Api {
    pub version: String,
    pub modules: Vec<Module>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub name: String,
    pub functions: Vec<Function>,
    #[serde(default)]
    pub structs: Vec<StructDef>,
    #[serde(default)]
    pub errors: Option<ErrorDomain>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeRef {
    #[serde(rename = "i32")]
    I32,
    #[serde(rename = "u32")]
    U32,
    #[serde(rename = "i64")]
    I64,
    #[serde(rename = "f64")]
    F64,
    #[serde(rename = "bool")]
    Bool,
    #[serde(rename = "string")]
    StringUtf8,
    #[serde(rename = "bytes")]
    Bytes,
    #[serde(rename = "handle")]
    Handle,
    Struct(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructDef {
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    pub fields: Vec<StructField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub doc: Option<String>,
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
        assert_eq!(json, r#"{"Struct":"Point"}"#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn struct_field_with_struct_type() {
        let field = StructField {
            name: "origin".to_string(),
            ty: TypeRef::Struct("Point".to_string()),
            doc: None,
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
    fn struct_def_clone_and_eq() {
        let s = StructDef {
            name: "Color".to_string(),
            doc: Some("RGB color".to_string()),
            fields: vec![
                StructField {
                    name: "r".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                },
                StructField {
                    name: "g".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                },
                StructField {
                    name: "b".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                },
            ],
        };
        assert_eq!(s, s.clone());
    }
}
