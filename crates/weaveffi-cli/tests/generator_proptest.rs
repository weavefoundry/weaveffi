//! Property test over every registered target: for random valid APIs, each
//! target renders without panicking, renders the same bytes twice, and
//! marks every file as generated, and the C header compiles.
//!
//! The APIs are written as IDL text (so the generator covers what a user
//! can write) from a small grammar: records, C-style and rich enums, an
//! error domain, an interface, a callback interface, and free functions
//! whose types draw on all of them, including optionals, lists, maps,
//! async, iterators, and `throws`. A case that fails validation is skipped,
//! so the grammar may be loose.

use std::fmt::Write as _;
use std::process::Command;

use proptest::prelude::*;
use weaveffi_cli::targets::REGISTRY;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

/// A type spelling over the declared names: `records` records, `enums`
/// C-style enums, one rich enum `Shape`, and one interface `Thing`.
fn ty(records: usize, enums: usize, depth: u32) -> BoxedStrategy<String> {
    let mut leaves: Vec<BoxedStrategy<String>> = vec![prop_oneof![
        Just("bool"),
        Just("i8"),
        Just("u8"),
        Just("i16"),
        Just("u16"),
        Just("i32"),
        Just("u32"),
        Just("i64"),
        Just("u64"),
        Just("f32"),
        Just("f64"),
        Just("string"),
        Just("bytes"),
        Just("Shape"),
        Just("Thing"),
    ]
    .prop_map(str::to_string)
    .boxed()];
    if records > 0 {
        leaves.push((0..records).prop_map(|r| format!("Rec{r}")).boxed());
    }
    if enums > 0 {
        leaves.push((0..enums).prop_map(|e| format!("Kind{e}")).boxed());
    }
    let leaf = proptest::strategy::Union::new(leaves).boxed();
    leaf.prop_recursive(depth, 8, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| format!("{t}?")),
            inner.clone().prop_map(|t| format!("[{t}]")),
            inner.prop_map(|t| format!("{{string:{t}}}")),
        ]
    })
    .boxed()
}

/// A field or parameter list of `n` entries named `a0`, `a1`, ....
fn fields(records: usize, enums: usize, max: usize) -> BoxedStrategy<String> {
    prop::collection::vec(ty(records, enums, 2), 0..=max)
        .prop_map(|types| {
            let items: Vec<String> = types
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{{ name: a{i}, type: \"{t}\" }}"))
                .collect();
            format!("[{}]", items.join(", "))
        })
        .boxed()
}

/// One function: its parameters, return, shape (sync, async, or iterator),
/// and whether it throws the error domain.
fn function(records: usize, enums: usize) -> BoxedStrategy<(String, Option<String>, u8, bool)> {
    (
        fields(records, enums, 3),
        prop::option::of(ty(records, enums, 2)),
        0u8..3,
        any::<bool>(),
    )
        .boxed()
}

fn api() -> impl Strategy<Value = String> {
    (0usize..3, 0usize..3).prop_flat_map(|(records, enums)| {
        (
            // Record fields reach only earlier records, so none contains
            // itself by value.
            (0..records)
                .map(|r| fields(r, enums, 3))
                .collect::<Vec<_>>(),
            prop::collection::vec(function(records, enums), 1..5),
            ty(records, enums, 1),
        )
            .prop_map(move |(record_fields, functions, method_ty)| {
                let mut y = String::from("version: \"0.13.0\"\n");
                if records > 0 {
                    y.push_str("records:\n");
                    for (r, f) in record_fields.iter().enumerate() {
                        // An empty record is invalid; give each a scalar.
                        let f = f.replacen('[', "[{ name: id, type: i32 }, ", 1).replace(", ]", "]");
                        let _ = writeln!(y, "  - {{ name: Rec{r}, fields: {f} }}");
                    }
                }
                y.push_str("enums:\n");
                for e in 0..enums {
                    let _ = writeln!(
                        y,
                        "  - {{ name: Kind{e}, variants: [{{ name: A, value: 0 }}, {{ name: B, value: 1 }}] }}"
                    );
                }
                y.push_str(
                    "  - name: Shape\n    variants:\n      - { name: Dot, value: 0 }\n      - { name: Circle, value: 1, fields: [{ name: r, type: f64 }] }\n",
                );
                y.push_str(
                    "errors:\n  - { name: Oops, codes: [{ name: Bad, code: 1, message: bad, fields: [{ name: why, type: string }] }] }\n",
                );
                let _ = write!(
                    y,
                    "interfaces:\n  - name: Thing\n    constructors: [{{ name: new, params: [] }}]\n    methods: [{{ name: get, params: [], return: \"{method_ty}\" }}]\n"
                );
                y.push_str(
                    "callbacks:\n  - { name: Hook, methods: [{ name: fire, params: [{ name: n, type: i32 }], return: \"string?\" }] }\n",
                );
                y.push_str("functions:\n");
                for (i, (params, ret, shape, throws)) in functions.iter().enumerate() {
                    let _ = write!(y, "  - {{ name: f{i}, params: {params}");
                    match (shape, ret) {
                        (1, ret) => {
                            y.push_str(", async: true");
                            if let Some(r) = ret {
                                let _ = write!(y, ", return: \"{r}\"");
                            }
                        }
                        (2, Some(r)) => {
                            let _ = write!(y, ", return: \"iter<{r}>\"");
                        }
                        (_, Some(r)) => {
                            let _ = write!(y, ", return: \"{r}\"");
                        }
                        _ => {}
                    }
                    if *throws {
                        y.push_str(", throws: Oops");
                    }
                    y.push_str(" }\n");
                }
                y.push_str("  - { name: hook, params: [{ name: h, type: Hook }] }\n");
                y
            })
    })
}

fn model(idl: &str) -> Option<Model> {
    let api = parse_api_str(idl, "yaml").ok()?;
    validate(&api, &Identity::named("prop"), None).ok()
}

fn compiles_as_c(files: &[(String, String)]) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents).map_err(|e| e.to_string())?;
    }
    let include = files
        .iter()
        .map(|(n, _)| n.clone())
        .find(|n| n.ends_with("_buffer.h"))
        .unwrap_or_else(|| "prop.h".to_string());
    let src = dir.path().join("main.c");
    std::fs::write(
        &src,
        format!("#include \"{include}\"\nint main(void) {{ return 0; }}\n"),
    )
    .map_err(|e| e.to_string())?;
    let out = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fsyntax-only",
            "-I",
        ])
        .arg(dir.path())
        .arg(&src)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn have_cc() -> bool {
    Command::new("cc").arg("--version").output().is_ok()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn every_target_renders_random_apis(idl in api()) {
        let Some(model) = model(&idl) else {
            return Ok(());
        };
        for desc in REGISTRY {
            let target = desc.build_default();
            let first = target.render(&model);
            let second = target.render(&model);
            prop_assert!(!first.is_empty(), "{} rendered nothing for\n{idl}", desc.name);
            prop_assert!(
                first.iter().zip(&second).all(|(a, b)| a.path == b.path && a.contents == b.contents)
                    && first.len() == second.len(),
                "{} isn't deterministic for\n{idl}",
                desc.name
            );
            for file in &first {
                let head: String = file.contents.lines().take(5).collect::<Vec<_>>().join("\n");
                prop_assert!(head.contains("DO NOT EDIT"), "{} in {} has no prelude", file.path, desc.name);
            }
            if desc.name == "c" && have_cc() {
                let files: Vec<(String, String)> = first
                    .iter()
                    .map(|f| (f.path.file_name().unwrap_or_default().to_string(), f.contents.clone()))
                    .collect();
                if let Err(e) = compiles_as_c(&files) {
                    prop_assert!(false, "the C header doesn't compile for\n{idl}\n{e}");
                }
            }
        }
    }
}
