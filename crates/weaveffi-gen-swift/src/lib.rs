//! Swift binding generator for WeaveFFI.
//!
//! Emits a SwiftPM package containing a thin Swift wrapper over the C ABI,
//! including module map, `Package.swift`, and Swift `async/await` shims for
//! functions marked `async: true`. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps cross the C ABI as value
//! buffers (one `const uint8_t*` + `size_t` pair in the WeaveFFI wire format).
//! The generated wrapper ships a small private writer/reader pair
//! (`WvWriter`/`WvReader`) plus one pack and one unpack routine per record and
//! rich enum, so records surface as plain Swift structs and rich enums as
//! native Swift enums with associated values. Objects surface as `final
//! class` wrappers owning one strong reference each, and callback interfaces
//! as Swift protocols whose implementations cross the boundary through a
//! process-wide vtable of `@convention(c)` trampolines.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::walk_modules;
use weaveffi_core::model::{BindingModel, ModuleBinding};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_ir::ir::Module;

use crate::entities::{render_swift_module_body, render_swift_module_types};
use crate::package::{
    render_modulemap, render_package_swift, render_packaged_package_swift, render_packaged_readme,
    resolve_module_name,
};
use crate::runtime::{
    render_buffer_runtime, render_callback_support, render_continuation_ref, render_error_infra,
};
use crate::types::{enum_raw_type, SwiftCtx};

/// Per-target configuration for [`SwiftGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwiftConfig {
    /// SwiftPM module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// function names, so `enum Kv` exposes `openStore` rather than
    /// `kvOpenStore`. Set to `false` to restore the module-prefixed spelling.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the Swift wrappers call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    /// Populated by the CLI; not user-configurable via `[swift]`.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for SwiftConfig {
    fn default() -> Self {
        Self {
            module_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl SwiftConfig {
    /// Returns the configured SwiftPM module name, falling back to
    /// `"WeaveFFI"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("WeaveFFI")
    }

    /// Returns the configured C ABI symbol prefix, falling back to
    /// `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Each module contributes ~2KB of Swift wrapper text on average (struct
/// declarations, codec routines, async wrappers); pre-allocating from this
/// estimate reduces `String` re-allocations as the wrapper grows past 64 KB.
const SWIFT_BASE_BYTES: usize = 4096;
const SWIFT_BYTES_PER_MODULE: usize = 2048;
const SWIFT_BYTES_PER_FUNCTION: usize = 512;
const SWIFT_BYTES_PER_STRUCT: usize = 512;

/// Estimate the rendered wrapper size from the module tree, for the initial
/// `String` allocation.
fn estimate_swift_capacity(modules: &[Module]) -> usize {
    fn count(modules: &[Module]) -> (usize, usize, usize) {
        let mut m = 0;
        let mut f = 0;
        let mut s = 0;
        for module in modules {
            m += 1;
            f += module.functions.len();
            s += module.structs.len();
            let (sm, sf, ss) = count(&module.modules);
            m += sm;
            f += sf;
            s += ss;
        }
        (m, f, s)
    }
    let (mods, funcs, structs) = count(modules);
    SWIFT_BASE_BYTES
        + mods * SWIFT_BYTES_PER_MODULE
        + funcs * SWIFT_BYTES_PER_FUNCTION
        + structs * SWIFT_BYTES_PER_STRUCT
}

/// Swift backend: emits a SwiftPM package with a thin Swift wrapper (module
/// map, `Package.swift`, and `async`/`await` shims) over the C ABI.
pub struct SwiftGenerator;

impl LanguageBackend for SwiftGenerator {
    type Config = SwiftConfig;

    fn name(&self) -> &'static str {
        "swift"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let module_name_owned = resolve_module_name(api, config);
        let module_name = module_name_owned.as_str();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("swift");
        let c_module = format!("C{module_name}");
        // The C shim is a SwiftPM `systemLibrary` target, so its module map
        // must live under `Sources/<target>/` for `swift build` to find it.
        let module_dir = dir.join("Sources").join(&c_module);

        let src_dir = dir.join("Sources").join(module_name);
        let swift_filename = format!("{module_name}.swift");
        vec![
            OutputFile::new(
                dir.join("Package.swift"),
                render_package_swift(module_name, &c_module, input_basename),
            ),
            OutputFile::new(
                module_dir.join("module.modulemap"),
                render_modulemap(&c_module, prefix, input_basename),
            ),
            OutputFile::new(
                src_dir.join(&swift_filename),
                render_swift_wrapper(
                    api,
                    model,
                    prefix,
                    config.strip_module_prefix,
                    input_basename,
                    &swift_filename,
                ),
            ),
        ]
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let module_name_owned = resolve_module_name(api, config);
        let module_name = module_name_owned.as_str();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("swift");
        let c_module = format!("C{module_name}");
        let xcframework = format!("{c_module}.xcframework");

        let src_dir = dir.join("Sources").join(module_name);
        let swift_filename = format!("{module_name}.swift");
        let wrapper = render_swift_wrapper(
            api,
            model,
            prefix,
            config.strip_module_prefix,
            input_basename,
            &swift_filename,
        );

        let mut files = vec![
            PackagedFile::text(
                dir.join("Package.swift"),
                render_packaged_package_swift(module_name, &c_module, &xcframework, input_basename),
            ),
            PackagedFile::text(src_dir.join(&swift_filename), wrapper),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(module_name, &c_module, prefix, ctx, input_basename),
            ),
        ];
        // Bundle the prebuilt libraries as xcframework-ready slices. Only
        // desktop slices are meaningful to a SwiftPM consumer; mobile and
        // WebAssembly builds belong to other package formats.
        for nb in ctx
            .binaries
            .binaries
            .iter()
            .filter(|nb| nb.platform.is_desktop())
        {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

/// Render the complete generated Swift wrapper file: prelude and imports,
/// the runtime helpers the model needs, every module's file-scope types, and
/// one namespace `enum` per top-level module.
fn render_swift_wrapper(
    api: &ResolvedApi,
    model: &BindingModel,
    c_prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let mut out = String::with_capacity(estimate_swift_capacity(&api.modules));
    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    // The C shim target is `C<module_name>` and the wrapper file is always
    // `<module_name>.swift`, so the system-library module to import is the
    // file stem with a `C` prefix. Deriving it here keeps the `import` in sync
    // with the module name picked from `[swift] module_name` / the IDL package.
    let module_name = filename.strip_suffix(".swift").unwrap_or(filename);
    out.push_str(&format!("import C{module_name}\nimport Foundation\n\n"));

    // Index the flat, pre-order model by its underscore-joined symbol path so
    // the recursive IR walk below can pull each module's precomputed C symbols
    // while still emitting the nested Swift `enum` structure the IR tree drives.
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    let all_mods = walk_modules(&api.modules).collect::<Vec<_>>();

    // Every module becomes a namespace `enum`; a wrapper type whose name
    // matches one of these is shadowed inside that namespace and must be
    // module-qualified at its use sites.
    let module_names: HashSet<String> = all_mods
        .iter()
        .map(|m| m.name.to_upper_camel_case())
        .collect();
    // Raw-value types of every C-style enum, so buffer codecs can emit the
    // right `i32` bit conversion for a referenced enum without re-resolving it.
    let enum_raws: HashMap<String, &'static str> = model
        .modules
        .iter()
        .flat_map(|m| m.enums.iter())
        .filter(|e| !e.is_rich())
        .map(|e| (e.name.clone(), enum_raw_type(e)))
        .collect();
    let ctx = SwiftCtx {
        c_prefix,
        swift_module: module_name,
        module_names: &module_names,
        enum_raws: &enum_raws,
    };

    render_error_infra(&mut out);
    render_buffer_runtime(&mut out);

    // Interface members can be async too, so consult every callable.
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    if has_async {
        render_continuation_ref(&mut out);
    }

    let has_callbacks = model
        .modules
        .iter()
        .any(|m| !m.callback_interfaces.is_empty());
    if has_callbacks {
        render_callback_support(&mut out);
    }

    for m in &api.modules {
        render_swift_module_types(&mut out, c_prefix, &by_path, m, &m.name, ctx);
        let type_name = m.name.to_upper_camel_case();
        out.push_str(&format!("public enum {} {{\n", type_name));
        render_swift_module_body(
            &mut out,
            c_prefix,
            &by_path,
            m,
            &m.name,
            1,
            strip_module_prefix,
            ctx,
        );
        out.push_str("}\n\n");
    }
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        Api, CallbackInterfaceDef, Function, InterfaceDef, Param, StructDef, StructField, TypeRef,
    };

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            doc: None,
        }
    }

    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
        }
    }

    fn named(name: &str) -> TypeRef {
        TypeRef::Named(name.into())
    }

    fn opt(ty: TypeRef) -> TypeRef {
        TypeRef::Optional(Box::new(ty))
    }

    /// One module exercising every object and callback shape the ABI
    /// contract distinguishes: an interface with a constructor and a method,
    /// nullable objects in and out, objects inside a record (directly and in
    /// a list), an iterator of objects, and a callback interface taking a
    /// string, an `i32`, a record, and an object, with `bool` and void
    /// returns.
    fn fixture() -> ResolvedApi {
        ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "shop".into(),
                doc: None,
                functions: vec![
                    func(
                        "find_cart",
                        vec![param("current", opt(named("Cart")))],
                        Some(opt(named("Cart"))),
                    ),
                    func(
                        "all_carts",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(named("Cart")))),
                    ),
                    func("watch", vec![param("watcher", named("Watcher"))], None),
                ],
                interfaces: vec![InterfaceDef {
                    name: "Cart".into(),
                    doc: None,
                    deprecated: None,
                    constructors: vec![func(
                        "new",
                        vec![param("owner", TypeRef::StringUtf8)],
                        None,
                    )],
                    methods: vec![func(
                        "add",
                        vec![param("sku", TypeRef::StringUtf8)],
                        Some(TypeRef::Bool),
                    )],
                    statics: vec![],
                }],
                callback_interfaces: vec![CallbackInterfaceDef {
                    name: "Watcher".into(),
                    doc: None,
                    deprecated: None,
                    methods: vec![
                        func(
                            "on_event",
                            vec![
                                param("name", TypeRef::StringUtf8),
                                param("count", TypeRef::I32),
                                param("order", named("Order")),
                                param("cart", named("Cart")),
                            ],
                            Some(TypeRef::Bool),
                        ),
                        func("on_done", vec![], None),
                    ],
                }],
                structs: vec![StructDef {
                    name: "Order".into(),
                    doc: None,
                    deprecated: None,
                    fields: vec![
                        StructField {
                            name: "cart".into(),
                            ty: named("Cart"),
                            doc: None,
                        },
                        StructField {
                            name: "history".into(),
                            ty: TypeRef::List(Box::new(named("Cart"))),
                            doc: None,
                        },
                    ],
                }],
                enums: vec![],
                errors: None,
                modules: vec![],
            }],
        })
    }

    fn render() -> String {
        let api = fixture();
        let model = BindingModel::build(&api, "weaveffi");
        render_swift_wrapper(
            &api,
            &model,
            "weaveffi",
            true,
            "weaveffi.yml",
            "WeaveFFI.swift",
        )
    }

    #[track_caller]
    fn assert_has(out: &str, needle: &str) {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }

    #[test]
    fn interface_class_owns_one_reference() {
        let out = render();
        assert_has(&out, "public final class Cart {");
        assert_has(&out, "let ptr: OpaquePointer");
        assert_has(&out, "weaveffi_shop_Cart_destroy(ptr)");
        assert_has(&out, "func clonePtr() -> OpaquePointer {");
        assert_has(&out, "weaveffi_shop_Cart_clone(ptr)!");
        // Exactly one destroy site: the deinit.
        assert_eq!(out.matches("weaveffi_shop_Cart_destroy(").count(), 1);
        // Constructor adopts the returned reference; the method borrows `ptr`.
        assert_has(&out, "public init(owner: String) {");
        assert_has(&out, "weaveffi_shop_Cart_new(");
        assert_has(&out, "public func add(sku: String) -> Bool {");
        assert_has(&out, "weaveffi_shop_Cart_add(ptr, ");
    }

    #[test]
    fn nullable_objects_map_to_optional_wrappers() {
        let out = render();
        assert_has(
            &out,
            "public static func findCart(current: Cart?) -> Cart? {",
        );
        assert_has(&out, "weaveffi_shop_find_cart(current?.ptr, &err)");
        assert_has(&out, "return rv.map { Cart(ptr: $0) }");
    }

    #[test]
    fn records_carry_object_tokens() {
        let out = render();
        assert_has(&out, "public struct Order {");
        assert_has(&out, "public var cart: Cart");
        assert_has(&out, "public var history: [Cart]");
        assert_has(&out, "w.writeObject(value.cart.clonePtr())");
        assert_has(&out, "w.writeObject(v0.clonePtr())");
        assert_has(&out, "Cart(ptr: r.readObject())");
        assert_has(&out, "mutating func writeObject(_ p: OpaquePointer)");
        assert_has(&out, "mutating func readObject() -> OpaquePointer {");
    }

    #[test]
    fn object_iterator_adopts_each_element() {
        let out = render();
        assert_has(
            &out,
            "public final class ShopAllCartsIterator: Sequence, IteratorProtocol {",
        );
        assert_has(&out, "public func next() -> Cart? {");
        assert_has(&out, "var item: OpaquePointer? = nil");
        assert_has(
            &out,
            "weaveffi_shop_AllCartsIterator_next(handle, &item, &err)",
        );
        assert_has(&out, "weaveffi_shop_AllCartsIterator_destroy(handle)");
        assert_has(&out, "return Cart(ptr: item!)");
    }

    #[test]
    fn callback_interface_renders_protocol_box_and_vtable() {
        let out = render();
        assert_has(&out, "public protocol Watcher {");
        assert_has(
            &out,
            "func onEvent(name: String, count: Int32, order: Order, cart: Cart) throws -> Bool",
        );
        assert_has(&out, "func onDone() throws\n");
        assert_has(&out, "final class WvWatcherBox {");
        assert_has(&out, "let impl: any Watcher");
        assert_has(&out, "enum WvWatcherVtable {");
        assert_has(&out, "static let value = weaveffi_shop_Watcher_vtable(");
        assert_has(
            &out,
            "static let pointer: UnsafePointer<weaveffi_shop_Watcher_vtable> = {",
        );
        assert_has(&out, "cell.initialize(to: value)");
        assert_has(
            &out,
            "on_event: { ctx, name, count, order_ptr, order_len, cart, out_err in",
        );
        assert_has(&out, "on_done: { ctx, out_err in");
        assert_has(&out, "free: { ctx in");
        assert_has(&out, "Unmanaged<WvWatcherBox>.fromOpaque(ctx!).release()");
        assert_has(
            &out,
            "Unmanaged<WvWatcherBox>.fromOpaque(ctx!).takeUnretainedValue()",
        );
        // No stray empty doc line when the interface is undocumented.
        assert!(!out.contains("\n///\n/// Implement this protocol"), "{out}");
    }

    #[test]
    fn callback_trampolines_decode_adopt_and_report_errors() {
        let out = render();
        // The record decodes through its codec before the call; the borrowed
        // string is copied and the object argument adopted at the call site.
        assert_has(
            &out,
            "[UInt8](UnsafeBufferPointer(start: order_ptr, count: order_len))",
        );
        assert_has(&out, "let v2 = wvReadOrder(&r1)");
        assert_has(
            &out,
            "return try wvBox.impl.onEvent(name: String(cString: name!), count: count, order: v2, cart: Cart(ptr: cart!))",
        );
        assert_has(&out, "try wvBox.impl.onDone()");
        // A thrown Swift error is reported as code -4 and a default returned.
        assert_has(&out, "weaveffi_error_set(outErr, -4, $0)");
        assert_has(&out, "wvForeignError(out_err, error)");
        assert_has(&out, "return false");
    }

    #[test]
    fn passing_a_callback_boxes_it_with_the_static_vtable() {
        let out = render();
        assert_has(&out, "public static func watch(watcher: Watcher) -> Void {");
        assert_has(
            &out,
            "let watcher_ctx = Unmanaged.passRetained(WvWatcherBox(watcher)).toOpaque()",
        );
        assert_has(
            &out,
            "weaveffi_shop_watch(watcher_ctx, WvWatcherVtable.pointer, &err)",
        );
    }

    #[test]
    fn no_arena_or_listener_surface_remains() {
        let out = render();
        assert!(!out.contains("arena"), "{out}");
        assert!(!out.contains("Listener"), "{out}");
        assert!(!out.contains("TypedHandle"), "{out}");
        assert!(!out.contains("weaveffi_arena"), "{out}");
    }
}
