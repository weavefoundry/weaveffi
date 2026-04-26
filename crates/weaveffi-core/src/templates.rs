use std::collections::HashMap;

use anyhow::{Context, Result};
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use tera::{Tera, Value};
use weaveffi_ir::ir::{Api, TypeRef};

pub struct TemplateEngine {
    tera: Tera,
}

impl Default for TemplateEngine {
    fn default() -> Self {
        let mut tera = Tera::default();
        register_case_filters(&mut tera);
        Self { tera }
    }
}

/// Register WeaveFFI's case-conversion filters on a Tera instance.
///
/// Templates can invoke these as `{{ name | to_snake_case }}`,
/// `{{ name | to_camel_case }}`, `{{ name | to_pascal_case }}`, or
/// `{{ name | to_shouty_snake_case }}` to adapt IR identifiers to the target
/// language's conventions without having to pre-process the context in Rust.
fn register_case_filters(tera: &mut Tera) {
    tera.register_filter("to_snake_case", case_filter(|s| s.to_snake_case()));
    tera.register_filter("to_camel_case", case_filter(|s| s.to_lower_camel_case()));
    tera.register_filter("to_pascal_case", case_filter(|s| s.to_upper_camel_case()));
    tera.register_filter(
        "to_shouty_snake_case",
        case_filter(|s| s.to_shouty_snake_case()),
    );
}

/// Wrap a `&str -> String` transformer in a closure that satisfies Tera's
/// `Filter` signature and rejects non-string inputs with a clear error.
fn case_filter(
    f: fn(&str) -> String,
) -> impl Fn(&Value, &HashMap<String, Value>) -> tera::Result<Value> + Sync + Send + 'static {
    move |value, _args| match value.as_str() {
        Some(s) => Ok(Value::String(f(s))),
        None => Err(tera::Error::msg(
            "case-conversion filter expects a string input",
        )),
    }
}

impl TemplateEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_builtin(&mut self, name: &str, content: &str) -> Result<()> {
        self.tera
            .add_raw_template(name, content)
            .with_context(|| format!("failed to load builtin template '{name}'"))
    }

    /// Load every `.tera` file under `dir` (recursively). Each template is
    /// registered under its path relative to `dir`, with `/` as the
    /// separator (e.g. `c/header.tera`), so nested layouts like
    /// `<dir>/c/header.tera` can override built-ins registered under the
    /// same name.
    pub fn load_dir(&mut self, dir: &Utf8Path) -> Result<()> {
        self.load_dir_rec(dir, dir)
    }

    fn load_dir_rec(&mut self, root: &Utf8Path, dir: &Utf8Path) -> Result<()> {
        let entries = std::fs::read_dir(dir.as_std_path())
            .with_context(|| format!("failed to read template directory '{dir}'"))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let utf8 = Utf8Path::from_path(&path)
                .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path: {}", path.display()))?
                .to_owned();
            if utf8.is_dir() {
                self.load_dir_rec(root, &utf8)?;
            } else if utf8.extension() == Some("tera") {
                let rel = utf8
                    .strip_prefix(root)
                    .with_context(|| format!("failed to strip template root from '{utf8}'"))?;
                let name = rel.as_str().replace('\\', "/");
                let content = std::fs::read_to_string(utf8.as_std_path())
                    .with_context(|| format!("failed to read template file '{utf8}'"))?;
                self.tera
                    .add_raw_template(&name, &content)
                    .with_context(|| format!("failed to parse template '{name}'"))?;
            }
        }
        Ok(())
    }

    pub fn has_template(&self, name: &str) -> bool {
        self.tera.get_template_names().any(|n| n == name)
    }

    pub fn render(&self, name: &str, context: &tera::Context) -> Result<String> {
        self.tera
            .render(name, context)
            .with_context(|| format!("failed to render template '{name}'"))
    }
}

pub fn type_ref_to_map(ty: &TypeRef) -> HashMap<String, tera::Value> {
    let mut map: HashMap<String, tera::Value> = HashMap::new();
    match ty {
        TypeRef::I32 => {
            map.insert("kind".into(), "i32".into());
        }
        TypeRef::U32 => {
            map.insert("kind".into(), "u32".into());
        }
        TypeRef::I64 => {
            map.insert("kind".into(), "i64".into());
        }
        TypeRef::F64 => {
            map.insert("kind".into(), "f64".into());
        }
        TypeRef::Bool => {
            map.insert("kind".into(), "bool".into());
        }
        TypeRef::StringUtf8 => {
            map.insert("kind".into(), "string".into());
        }
        TypeRef::Bytes => {
            map.insert("kind".into(), "bytes".into());
        }
        TypeRef::BorrowedStr => {
            map.insert("kind".into(), "borrowed_str".into());
        }
        TypeRef::BorrowedBytes => {
            map.insert("kind".into(), "borrowed_bytes".into());
        }
        TypeRef::Handle => {
            map.insert("kind".into(), "handle".into());
        }
        TypeRef::TypedHandle(name) => {
            map.insert("kind".into(), "handle".into());
            map.insert("name".into(), name.clone().into());
        }
        TypeRef::Struct(name) => {
            map.insert("kind".into(), "struct".into());
            map.insert("name".into(), name.clone().into());
        }
        TypeRef::Enum(name) => {
            map.insert("kind".into(), "enum".into());
            map.insert("name".into(), name.clone().into());
        }
        TypeRef::Optional(inner) => {
            map.insert("kind".into(), "optional".into());
            map.insert(
                "inner".into(),
                serde_json::to_value(type_ref_to_map(inner)).unwrap(),
            );
        }
        TypeRef::List(inner) => {
            map.insert("kind".into(), "list".into());
            map.insert(
                "inner".into(),
                serde_json::to_value(type_ref_to_map(inner)).unwrap(),
            );
        }
        TypeRef::Map(key, value) => {
            map.insert("kind".into(), "map".into());
            map.insert(
                "key".into(),
                serde_json::to_value(type_ref_to_map(key)).unwrap(),
            );
            map.insert(
                "value".into(),
                serde_json::to_value(type_ref_to_map(value)).unwrap(),
            );
        }
        TypeRef::Iterator(inner) => {
            map.insert("kind".into(), "iterator".into());
            map.insert(
                "inner".into(),
                serde_json::to_value(type_ref_to_map(inner)).unwrap(),
            );
        }
        TypeRef::Callback(name) => {
            map.insert("kind".into(), "callback".into());
            map.insert("name".into(), name.clone().into());
        }
    }
    map
}

pub fn api_to_context(api: &Api) -> tera::Context {
    let mut ctx = tera::Context::new();
    ctx.insert("version", &api.version);

    let modules: Vec<tera::Value> = api
        .modules
        .iter()
        .map(|module| {
            let functions: Vec<tera::Value> = module
                .functions
                .iter()
                .map(|func| {
                    let params: Vec<tera::Value> = func
                        .params
                        .iter()
                        .map(|p| {
                            serde_json::json!({
                                "name": p.name,
                                "type": type_ref_to_map(&p.ty),
                            })
                        })
                        .collect();

                    let returns = func
                        .returns
                        .as_ref()
                        .map(|r| serde_json::to_value(type_ref_to_map(r)).unwrap());

                    serde_json::json!({
                        "name": func.name,
                        "params": params,
                        "returns": returns,
                        "doc": func.doc,
                    })
                })
                .collect();

            let structs: Vec<tera::Value> = module
                .structs
                .iter()
                .map(|s| {
                    let fields: Vec<tera::Value> = s
                        .fields
                        .iter()
                        .map(|field| {
                            serde_json::json!({
                                "name": field.name,
                                "type": type_ref_to_map(&field.ty),
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "name": s.name,
                        "fields": fields,
                    })
                })
                .collect();

            let enums: Vec<tera::Value> = module
                .enums
                .iter()
                .map(|e| {
                    let variants: Vec<tera::Value> = e
                        .variants
                        .iter()
                        .map(|v| {
                            serde_json::json!({
                                "name": v.name,
                                "value": v.value,
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "name": e.name,
                        "variants": variants,
                    })
                })
                .collect();

            serde_json::json!({
                "name": module.name,
                "functions": functions,
                "structs": structs,
                "enums": enums,
            })
        })
        .collect();

    ctx.insert("modules", &modules);
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Function, Module, Param, StructDef, StructField};

    #[test]
    fn template_context_callback_no_panic() {
        let map = type_ref_to_map(&TypeRef::Callback("OnData".into()));
        assert_eq!(map["kind"], "callback");
        assert_eq!(map["name"], "OnData");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn api_context_has_modules() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "math".into(),
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![
                        Param {
                            name: "a".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: Some("Add two numbers".into()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Point".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let ctx = api_to_context(&api);
        let json = ctx.into_json();

        assert_eq!(json["version"], "0.1.0");

        let modules = json["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["name"], "math");

        let funcs = modules[0]["functions"].as_array().unwrap();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0]["name"], "add");
        assert_eq!(funcs[0]["doc"], "Add two numbers");
        assert_eq!(funcs[0]["returns"]["kind"], "i32");

        let params = funcs[0]["params"].as_array().unwrap();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0]["name"], "a");
        assert_eq!(params[0]["type"]["kind"], "i32");

        let structs = modules[0]["structs"].as_array().unwrap();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0]["name"], "Point");

        let fields = structs[0]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["name"], "x");
        assert_eq!(fields[0]["type"]["kind"], "f64");
    }

    #[test]
    fn type_ref_context_struct() {
        let map = type_ref_to_map(&TypeRef::Struct("Point".into()));
        assert_eq!(map["kind"], "struct");
        assert_eq!(map["name"], "Point");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn template_render_basic() {
        let mut engine = TemplateEngine::new();
        engine.load_builtin("greeting", "hello {{ name }}").unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("name", "world");

        let output = engine.render("greeting", &ctx).unwrap();
        assert_eq!(output, "hello world");
    }

    #[test]
    fn load_dir_overrides_builtin() {
        let mut engine = TemplateEngine::new();
        engine
            .load_builtin("test.tera", "original {{ val }}")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(dir_path.join("test.tera"), "override {{ val }}").unwrap();

        engine.load_dir(dir_path).unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("val", "ok");
        let output = engine.render("test.tera", &ctx).unwrap();
        assert_eq!(output, "override ok");
    }

    #[test]
    fn load_dir_recurses_into_subdirs_and_uses_forward_slash_names() {
        let mut engine = TemplateEngine::new();
        engine
            .load_builtin("c/header.tera", "builtin {{ val }}")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::create_dir_all(dir_path.join("c")).unwrap();
        std::fs::write(dir_path.join("c").join("header.tera"), "nested {{ val }}").unwrap();

        engine.load_dir(dir_path).unwrap();

        assert!(engine.has_template("c/header.tera"));

        let mut ctx = tera::Context::new();
        ctx.insert("val", "ok");
        let output = engine.render("c/header.tera", &ctx).unwrap();
        assert_eq!(output, "nested ok");
    }

    #[test]
    fn has_template_reports_registered_names() {
        let mut engine = TemplateEngine::new();
        assert!(!engine.has_template("c/header.tera"));
        engine.load_builtin("c/header.tera", "hi").unwrap();
        assert!(engine.has_template("c/header.tera"));
        assert!(!engine.has_template("c/missing.tera"));
    }

    #[test]
    fn case_filters_transform_strings() {
        let mut engine = TemplateEngine::new();
        engine
            .load_builtin(
                "cases",
                "{{ v | to_snake_case }}|{{ v | to_camel_case }}|{{ v | to_pascal_case }}|{{ v | to_shouty_snake_case }}",
            )
            .unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("v", "HelloWorld");

        let output = engine.render("cases", &ctx).unwrap();
        assert_eq!(output, "hello_world|helloWorld|HelloWorld|HELLO_WORLD");
    }

    #[test]
    fn case_filters_round_trip_snake_input() {
        let mut engine = TemplateEngine::new();
        engine
            .load_builtin(
                "cases",
                "{{ v | to_snake_case }}|{{ v | to_camel_case }}|{{ v | to_pascal_case }}",
            )
            .unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("v", "my_function_name");

        let output = engine.render("cases", &ctx).unwrap();
        assert_eq!(output, "my_function_name|myFunctionName|MyFunctionName");
    }

    #[test]
    fn case_filter_rejects_non_string_input() {
        let mut engine = TemplateEngine::new();
        engine
            .load_builtin("nums", "{{ v | to_snake_case }}")
            .unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("v", &42_i64);

        let err = engine.render("nums", &ctx).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("case-conversion filter expects a string input"),
            "unexpected error chain: {chain}"
        );
    }
}
