//! Checks for the C target's `{library}_buffer.h` value-buffer helper
//! header: which files the generator emits, the shapes it renders, and that
//! its codecs round-trip values and reject malformed buffers when compiled as
//! C and as C++. A missing compiler skips its half of the round trip locally
//! and fails the test under CI (`CI=true`).

use std::process::Command;

use camino::Utf8Path;
use weaveffi_cli::targets;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

const SHOP: &str = r#"
version: "0.12.0"
modules:
  - name: shop
    enums:
      - name: Size
        variants:
          - { name: Small, value: 1 }
          - { name: Large, value: 2 }
      - name: Price
        variants:
          - { name: Free, value: 0 }
          - name: Fixed
            value: 1
            fields:
              - { name: cents, type: i64 }
          - name: Note
            value: 2
            fields:
              - { name: text, type: string }
    structs:
      - name: Item
        fields:
          - { name: name, type: string }
          - { name: size, type: Size }
          - { name: tags, type: "[string]" }
          - { name: stock, type: "{string:i32}" }
          - { name: discount, type: "i32?" }
          - { name: price, type: Price }
          - { name: blob, type: bytes }
          - { name: matrix, type: "[[u8]]" }
    errors:
      - name: ShopError
        codes:
          - name: OutOfStock
            code: 1
            message: "out of stock"
            fields:
              - { name: missing, type: "[string]" }
    functions:
      - name: echo
        params:
          - { name: item, type: Item }
        return: "Item?"
        throws: ShopError
"#;

const PLAIN: &str = r#"
version: "0.12.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
"#;

fn load(yaml: &str, name: &str) -> Model {
    let api = parse_api_str(yaml, "yaml").expect("parse");
    validate(&api, &Identity::named(name), None).expect("validate")
}

/// The C target with the `[generators.c]` table `config`.
fn c_target(config: &str) -> Box<dyn targets::Target> {
    targets::find("c")
        .expect("the C target")
        .build(toml::from_str(config).expect("a TOML table"))
        .expect("a valid C configuration")
}

fn generate(model: &Model, config: &str) -> Vec<(String, String)> {
    c_target(config)
        .render(model)
        .into_iter()
        .map(|f| {
            let name = f.path.file_name().unwrap_or_default().to_string();
            (name, f.contents)
        })
        .collect()
}

fn buffer_header(yaml: &str, name: &str) -> String {
    let api = load(yaml, name);
    let files = generate(&api, "");
    files
        .into_iter()
        .find(|(n, _)| n == &format!("{name}_buffer.h"))
        .map(|(_, c)| c)
        .expect("buffer header emitted")
}

#[test]
fn emits_the_helper_header_only_when_buffers_cross_the_abi() {
    let api = load(SHOP, "shop");
    let names: Vec<String> = generate(&api, "").into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["shop.h", "shop_buffer.h"]);

    let names: Vec<String> = generate(&api, "buffer_helpers = false")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, ["shop.h"]);

    let plain = load(PLAIN, "math");
    let names: Vec<String> = generate(&plain, "").into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["math.h"]);
}

#[test]
fn config_rejects_unknown_keys() {
    let table = toml::from_str("prefix = \"x\"").unwrap();
    assert!(targets::find("c").unwrap().build(table).is_err());
}

#[test]
fn renders_structs_unions_and_payloads() {
    let h = buffer_header(SHOP, "shop");
    assert!(h.contains("#include \"shop.h\""));
    assert!(h.contains("struct shop_shop_Item {\n    shop_str name;\n    shop_shop_Size size;"));
    assert!(h.contains("    int32_t* discount;\n"));
    assert!(h.contains("struct shop_list_string {\n    shop_str* items;\n    size_t len;\n};"));
    assert!(h.contains("struct shop_map_string_i32 {\n    shop_str* keys;\n    int32_t* values;"));
    assert!(h.contains("    shop_shop_Price_Tag tag;\n    union {"));
    assert!(h.contains("        struct {\n            int64_t cents;\n        } Fixed;"));
    assert!(h.contains("struct shop_shop_ShopError_OutOfStock_payload {"));
    assert!(h.contains(
        "static inline bool shop_shop_ShopError_OutOfStock_payload_decode(const uint8_t* ptr, size_t len, shop_shop_ShopError_OutOfStock_payload* out);"
    ));
    assert!(h.contains(
        "static inline void shop_opt_Item_write(shop_writer* w, const shop_shop_Item* v);"
    ));
    // The price union is defined before the record that embeds it.
    let price = h.find("struct shop_shop_Price {").expect("price struct");
    let item = h.find("struct shop_shop_Item {").expect("item struct");
    assert!(price < item);
    // Nothing is branded after the tool.
    assert!(!h.contains("weaveffi_"));
}

/// A C program that encodes an `Item`, decodes it back, and checks every
/// field (including a string with an interior NUL), then feeds the decoder
/// malformed buffers.
const ROUNDTRIP_C: &str = r#"
#include <assert.h>
#include <stdio.h>
#include "shop_buffer.h"

int main(void) {
    shop_str tags[2] = {{"a\0b", 3}, {"", 0}};
    shop_str keys[1] = {{"k", 1}};
    int32_t counts[1] = {-7};
    int32_t discount = 15;
    uint8_t row0[2] = {1, 2};
    shop_bytes rows[2] = {{row0, 2}, {NULL, 0}};
    shop_shop_Item item;
    memset(&item, 0, sizeof item);
    item.name = shop_str_of("caf\xC3\xA9");
    item.size = shop_shop_Size_Large;
    item.tags.items = tags;
    item.tags.len = 2;
    item.stock.keys = keys;
    item.stock.values = counts;
    item.stock.len = 1;
    item.discount = &discount;
    item.price.tag = shop_shop_Price_Note;
    item.price.as.Note.text = shop_str_of("half");
    item.blob = shop_bytes_of(row0, 2);
    item.matrix.items = rows;
    item.matrix.len = 2;

    shop_writer w;
    memset(&w, 0, sizeof w);
    shop_opt_Item_write(&w, &item);
    assert(!w.failed);

    shop_shop_Item* back = NULL;
    assert(shop_opt_Item_decode(w.ptr, w.len, &back));
    assert(back != NULL);
    assert(back->name.len == 5 && memcmp(back->name.ptr, "caf\xC3\xA9", 5) == 0);
    assert(back->name.ptr[5] == '\0');
    assert(back->size == shop_shop_Size_Large);
    assert(back->tags.len == 2 && back->tags.items[0].len == 3);
    assert(memcmp(back->tags.items[0].ptr, "a\0b", 3) == 0);
    assert(back->tags.items[1].len == 0 && back->tags.items[1].ptr[0] == '\0');
    assert(back->stock.len == 1 && back->stock.values[0] == -7);
    assert(back->discount != NULL && *back->discount == 15);
    assert(back->price.tag == shop_shop_Price_Note);
    assert(strcmp(back->price.as.Note.text.ptr, "half") == 0);
    assert(back->blob.len == 2 && back->blob.ptr[1] == 2);
    assert(back->matrix.len == 2 && back->matrix.items[0].len == 2 && back->matrix.items[1].len == 0);
    shop_opt_Item_free(&back);
    assert(back == NULL);

    /* Truncation anywhere fails cleanly and leaves nothing allocated. */
    for (size_t cut = 0; cut < w.len; cut++) {
        shop_shop_Item* partial = NULL;
        assert(!shop_opt_Item_decode(w.ptr, cut, &partial));
        assert(partial == NULL);
    }
    /* Trailing bytes are rejected. */
    shop_writer_put_u8(&w, 0);
    assert(!shop_opt_Item_decode(w.ptr, w.len, &back));
    shop_writer_free(&w);

    /* An absent optional, a bad presence byte, a bad tag, and a count the
       buffer can't hold. */
    const uint8_t absent[1] = {0};
    assert(shop_opt_Item_decode(absent, 1, &back) && back == NULL);
    const uint8_t bad_flag[1] = {2};
    assert(!shop_opt_Item_decode(bad_flag, 1, &back));
    const uint8_t bad_tag[4] = {9, 0, 0, 0};
    shop_shop_Price price;
    assert(!shop_shop_Price_decode(bad_tag, 4, &price));
    const uint8_t huge[4] = {0xff, 0xff, 0xff, 0x7f};
    shop_list_string list;
    assert(!shop_list_string_decode(huge, 4, &list));
    assert(list.items == NULL && list.len == 0);

    /* A string must be well-formed UTF-8: a stray continuation byte, an
       overlong encoding, and an encoded surrogate all fail the decoder. */
    const uint8_t bad_utf8[3][7] = {
        {3, 0, 0, 0, 'a', 0x80, 'b'},
        {3, 0, 0, 0, 0xC0, 0xAF, 'b'},
        {3, 0, 0, 0, 0xED, 0xA0, 0x80},
    };
    for (int i = 0; i < 3; i++) {
        shop_list_string one;
        uint8_t buf[11] = {1, 0, 0, 0};
        memcpy(buf + 4, bad_utf8[i], 7);
        assert(!shop_list_string_decode(buf, 11, &one));
        assert(one.items == NULL && one.len == 0);
    }
    const uint8_t good_utf8[11] = {1, 0, 0, 0, 3, 0, 0, 0, 0xE2, 0x82, 0xAC};
    shop_list_string euro;
    assert(shop_list_string_decode(good_utf8, 11, &euro));
    assert(euro.len == 1 && euro.items[0].len == 3);
    shop_list_string_free(&euro);

    /* Error payloads decode through their own struct. */
    shop_writer pw;
    memset(&pw, 0, sizeof pw);
    shop_shop_ShopError_OutOfStock_payload payload;
    memset(&payload, 0, sizeof payload);
    payload.missing.items = tags;
    payload.missing.len = 1;
    shop_shop_ShopError_OutOfStock_payload_write(&pw, &payload);
    shop_shop_ShopError_OutOfStock_payload decoded;
    assert(shop_shop_ShopError_OutOfStock_payload_decode(pw.ptr, pw.len, &decoded));
    assert(decoded.missing.len == 1 && decoded.missing.items[0].len == 3);
    shop_shop_ShopError_OutOfStock_payload_free(&decoded);
    shop_writer_free(&pw);

    puts("ok");
    return 0;
}
"#;

/// Whether `tool` runs. A missing tool is a printed skip locally and a
/// failure under CI, where every runner has both compilers.
fn have(tool: &str) -> bool {
    let found = Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "{tool} not found (required when CI=true)",
        );
        eprintln!("{tool} not found; skipping its round trip");
    }
    found
}

#[test]
fn codecs_round_trip_in_c_and_cpp() {
    let compilers = [("cc", "c", "-std=c11"), ("c++", "cpp", "-std=c++17")];
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = Utf8Path::from_path(tmp.path()).expect("utf8");
    let api = load(SHOP, "shop");
    for (name, contents) in generate(&api, "") {
        std::fs::write(dir.join(name), contents).expect("write header");
    }
    for (cc, ext, std) in compilers {
        if !have(cc) {
            continue;
        }
        let src = dir.join(format!("roundtrip.{ext}"));
        let exe = dir.join(format!("roundtrip_{ext}"));
        std::fs::write(&src, ROUNDTRIP_C).expect("write source");
        // MinGW (the `cc` on Windows runners) ships no sanitizer runtimes.
        let sanitize: &[&str] = if cfg!(windows) {
            &[]
        } else {
            &["-fsanitize=address,undefined"]
        };
        let out = Command::new(cc)
            .args([std, "-Wall", "-Wextra", "-Werror"])
            .args(sanitize)
            .arg("-I")
            .arg(dir.as_str())
            .arg(src.as_str())
            .arg("-o")
            .arg(exe.as_str())
            .output()
            .expect("run compiler");
        assert!(
            out.status.success(),
            "{cc} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(exe.as_str()).output().expect("run program");
        assert!(
            run.status.success(),
            "{cc} program failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

/// Every snapshot fixture's headers compile cleanly as C11 and C++17, so a
/// declaration shape the kitchen-sink snapshot doesn't show (reserved
/// words, deep nesting, rich enums) can't produce an invalid header.
#[test]
fn fixture_headers_compile_in_c_and_cpp() {
    let fixtures = [
        "kitchen_sink",
        "shapes",
        "nested_modules",
        "docs_everywhere",
        "edge_cases",
    ];
    let compilers = [("cc", "c", "-std=c11"), ("c++", "cpp", "-std=c++17")];
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = Utf8Path::from_path(tmp.path()).expect("utf8");
    for stem in fixtures {
        let path = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("{stem}.yml"));
        let yaml = std::fs::read_to_string(&path).expect("read fixture");
        let files = generate(&load(&yaml, stem), "");
        let include = files
            .iter()
            .map(|(n, _)| n.clone())
            .find(|n| n.ends_with("_buffer.h"))
            .unwrap_or_else(|| format!("{stem}.h"));
        for (name, contents) in &files {
            std::fs::write(dir.join(name), contents).expect("write header");
        }
        for (cc, ext, std) in compilers {
            if !have(cc) {
                continue;
            }
            let src = dir.join(format!("{stem}.{ext}"));
            std::fs::write(
                &src,
                format!("#include \"{include}\"\nint main(void) {{ return 0; }}\n"),
            )
            .expect("write source");
            let out = Command::new(cc)
                .args([std, "-Wall", "-Wextra", "-Werror", "-pedantic"])
                .arg("-I")
                .arg(dir.as_str())
                .arg("-c")
                .arg(src.as_str())
                .arg("-o")
                .arg(dir.join(format!("{stem}_{ext}.o")).as_str())
                .output()
                .expect("run compiler");
            assert!(
                out.status.success(),
                "{cc} failed on {stem}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
