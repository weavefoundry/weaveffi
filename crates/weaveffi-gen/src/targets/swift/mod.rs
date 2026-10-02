//! Swift binding generator.
//!
//! Emits a standalone SwiftPM package: the Swift wrapper module over the C
//! ABI, plus a `C{Module}` system-library target holding a copy of the C
//! header and a module map that links the native library. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! Records, rich enums, optionals, lists, and maps cross the C ABI as value
//! buffers. The wrapper ships a small private writer/reader pair
//! (`WvWriter`/`WvReader`, decoding in place from the library's memory) plus
//! one pack and one unpack routine per record and rich enum, so records
//! surface as plain Swift structs and rich enums as native Swift enums with
//! associated values. Objects surface as `final class` wrappers owning one
//! strong reference each, callback interfaces as class-bound `Sendable`
//! protocols whose implementations cross the boundary through a process-wide
//! vtable of `@convention(c)` trampolines, and async functions as `async`
//! functions over checked continuations, with task cancellation wired to the
//! native cancel token.
//!
//! The fixed Swift sources (runtime, manifest, module map, README) live
//! under `runtime/` and are spliced with the package's names.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::targets::c::{header_name, render_c_header_from_model};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::swift::entities::{render_swift_module_types, render_swift_namespace};
use crate::targets::swift::package::{
    render_modulemap, render_package_swift, render_packaged_readme,
};
use crate::targets::swift::runtime::{render_runtime, Names};
use crate::targets::swift::types::SwiftCtx;

/// Per-target configuration for [`SwiftGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwiftConfig {
    /// SwiftPM package, product, and module name. Defaults to the package
    /// name in PascalCase (`kvstore` becomes `Kvstore`); the C module is
    /// always this name with a `C` prefix.
    pub module_name: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// function names, so `enum Kv` exposes `openStore` rather than
    /// `kvOpenStore`. Set to `false` to restore the module-prefixed spelling.
    pub strip_module_prefix: bool,
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
            input_basename: None,
        }
    }
}

impl SwiftConfig {
    /// The input IDL basename embedded in generated file headers, falling
    /// back to `"api.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// The resolved names of one generated package.
struct Layout {
    /// The Swift module (and package and product) name.
    module: String,
    /// The C module name, `C{module}`.
    c_module: String,
    /// The native library base name.
    library: String,
    /// The C header file name.
    header: String,
}

impl Layout {
    fn new(api: &ResolvedApi, config: &SwiftConfig) -> Self {
        let identity = api.identity();
        let module = config
            .module_name
            .clone()
            .unwrap_or_else(|| identity.pascal_name());
        Self {
            c_module: format!("C{module}"),
            module,
            library: identity.library.clone(),
            header: header_name(api),
        }
    }

    fn names(&self) -> Names<'_> {
        Names {
            module: &self.module,
            c_module: &self.c_module,
            library: &self.library,
            header: &self.header,
        }
    }

    /// The error type for codes outside any declared domain.
    fn runtime_error(&self) -> String {
        format!("{}RuntimeError", self.module)
    }

    /// The `(path, contents)` of every source file of the package rooted at
    /// `dir`.
    fn sources(
        &self,
        model: &BindingModel,
        dir: &Utf8Path,
        config: &SwiftConfig,
    ) -> Vec<(Utf8PathBuf, String)> {
        let input_basename = config.input_basename();
        let c_dir = dir.join("Sources").join(&self.c_module);
        let swift_file = format!("{}.swift", self.module);
        vec![
            (
                dir.join("Package.swift"),
                render_package_swift(&self.names(), input_basename),
            ),
            (
                c_dir.join("module.modulemap"),
                render_modulemap(&self.names(), input_basename),
            ),
            (
                c_dir.join(&self.header),
                render_c_header_from_model(model, input_basename, &self.header),
            ),
            (
                dir.join("Sources").join(&self.module).join(&swift_file),
                render_swift_wrapper(self, model, config, &swift_file),
            ),
        ]
    }
}

/// Swift backend: emits a standalone SwiftPM package wrapping the C ABI.
pub struct SwiftGenerator;

impl LanguageBackend for SwiftGenerator {
    type Config = SwiftConfig;

    fn name(&self) -> &'static str {
        "swift"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        Layout::new(api, config)
            .sources(model, &out_dir.join("swift"), config)
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect()
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let layout = Layout::new(api, config);
        let dir = out_dir.join("swift");
        let mut files: Vec<PackagedFile> = layout
            .sources(model, &dir, config)
            .into_iter()
            .map(|(path, contents)| PackagedFile::text(path, contents))
            .collect();
        files.push(PackagedFile::text(
            dir.join("README.md"),
            render_packaged_readme(&layout.names(), ctx, config.input_basename()),
        ));
        // Desktop slices only: they link through the system-library target
        // or fuse into the XCFramework the README describes.
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

/// Render the complete Swift wrapper file: prelude and imports, the private
/// runtime, every module's file-scope types, and one namespace `enum` per
/// top-level module.
fn render_swift_wrapper(
    layout: &Layout,
    model: &BindingModel,
    config: &SwiftConfig,
    filename: &str,
) -> String {
    let runtime_error = layout.runtime_error();
    let ctx = SwiftCtx::new(model, &layout.module);
    let mut out = String::with_capacity(16 * 1024);
    out.push_str(&render_prelude(
        CommentStyle::DoubleSlash,
        config.input_basename(),
    ));
    out.push_str(&format!(
        "import {}\nimport Foundation\n\n",
        layout.c_module
    ));
    out.push_str(&render_runtime(model, &layout.names(), &runtime_error));
    out.push_str("\n// MARK: - API\n\n");
    for mb in &model.modules {
        render_swift_module_types(&mut out, mb, &ctx);
    }
    for root in model.roots() {
        render_swift_namespace(&mut out, model, root, 0, config.strip_module_prefix, &ctx);
        out.push('\n');
    }
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_model::ir::{
        Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
        TypeRef,
    };
    use weaveffi_model::pkg::Identity;

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
    /// contract distinguishes, plus a cancellable async function.
    fn fixture() -> ResolvedApi {
        let mut wait = func("wait", vec![param("ms", TypeRef::I64)], Some(TypeRef::I64));
        wait.r#async = true;
        wait.cancellable = true;
        ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "shop".into(),
                doc: None,
                functions: vec![
                    func(
                        "find_cart",
                        vec![param("current", opt(named("shop.Cart")))],
                        Some(opt(named("shop.Cart"))),
                    ),
                    func(
                        "all_carts",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(named("shop.Cart")))),
                    ),
                    func("watch", vec![param("watcher", named("shop.Watcher"))], None),
                    func(
                        "echo",
                        vec![param("text", TypeRef::StringUtf8)],
                        Some(TypeRef::StringUtf8),
                    ),
                    wait,
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
                                param("order", named("shop.Order")),
                                param("cart", named("shop.Cart")),
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
                            ty: named("shop.Cart"),
                            doc: None,
                        },
                        StructField {
                            name: "history".into(),
                            ty: TypeRef::List(Box::new(named("shop.Cart"))),
                            doc: None,
                        },
                    ],
                }],
                enums: vec![],
                errors: None,
                modules: vec![],
            }],
        })
        .with_identity(Identity::named("shop_kit"))
    }

    fn render() -> String {
        let api = fixture();
        let model = BindingModel::build(&api);
        let config = SwiftConfig::default();
        let layout = Layout::new(&api, &config);
        render_swift_wrapper(&layout, &model, &config, "ShopKit.swift")
    }

    #[track_caller]
    fn assert_has(out: &str, needle: &str) {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }

    #[test]
    fn names_come_from_the_identity() {
        let api = fixture();
        let model = BindingModel::build(&api);
        let config = SwiftConfig::default();
        let files = Layout::new(&api, &config).sources(&model, Utf8Path::new("out"), &config);
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "out/Package.swift",
                "out/Sources/CShopKit/module.modulemap",
                "out/Sources/CShopKit/shop_kit.h",
                "out/Sources/ShopKit/ShopKit.swift",
            ]
        );
        assert_has(&files[0].1, "name: \"ShopKit\"");
        assert_has(&files[0].1, ".systemLibrary(name: \"CShopKit\")");
        assert_has(
            &files[0].1,
            ".binaryTarget(name: \"CShopKit\", path: xcframework)",
        );
        assert_has(&files[1].1, "header \"shop_kit.h\"");
        assert_has(&files[1].1, "link \"shop_kit\"");
        let all = files.iter().map(|(_, c)| c.as_str()).collect::<String>();
        assert!(!all.contains("weaveffi_"), "{all}");
        assert!(!all.contains("WeaveFFIError"), "{all}");
    }

    #[test]
    fn module_name_override_wins() {
        let api = fixture();
        let config = SwiftConfig {
            module_name: Some("Shop".into()),
            ..SwiftConfig::default()
        };
        let layout = Layout::new(&api, &config);
        assert_eq!(layout.module, "Shop");
        assert_eq!(layout.c_module, "CShop");
        assert_eq!(layout.library, "shop_kit");
    }

    #[test]
    fn contract_is_checked_before_the_first_call() {
        let out = render();
        assert_has(&out, "let abi = shop_kit_abi_version()");
        assert_has(&out, "wvCheckModule(\"shop\", shop_kit_shop_checksum(), 0x");
        assert_has(&out, "public struct ShopKitRuntimeError: Error");
        assert_has(
            &out,
            "public static func echo(text: String) -> String {\n        wvLoad()",
        );
    }

    #[test]
    fn strings_cross_as_pointer_and_length() {
        let out = render();
        assert_has(&out, "wvWithUTF8(text) { text_ptr, text_len in");
        assert_has(
            &out,
            "shop_kit_shop_echo(text_ptr, text_len, &outLen, &err)",
        );
        assert_has(&out, "return wvTakeString(rv, outLen)");
    }

    #[test]
    fn interface_class_owns_one_reference() {
        let out = render();
        assert_has(&out, "public final class Cart: @unchecked Sendable {");
        assert_has(&out, "let ptr: OpaquePointer");
        assert_has(&out, "shop_kit_shop_Cart_destroy(ptr)");
        assert_has(&out, "wvNonNull(shop_kit_shop_Cart_clone(ptr))");
        assert_eq!(out.matches("shop_kit_shop_Cart_destroy(").count(), 1);
        assert_has(&out, "public init(owner: String) {");
        assert_has(&out, "self.ptr = wvNonNull(rv)");
        assert_has(&out, "public func add(sku: String) -> Bool {");
        assert_has(&out, "shop_kit_shop_Cart_add(ptr, sku_ptr, sku_len, &err)");
    }

    #[test]
    fn nullable_objects_map_to_optional_wrappers() {
        let out = render();
        assert_has(
            &out,
            "public static func findCart(current: Cart?) -> Cart? {",
        );
        assert_has(&out, "shop_kit_shop_find_cart(current?.ptr, &err)");
        assert_has(&out, "return rv.map { Cart(ptr: $0) }");
    }

    #[test]
    fn records_carry_object_tokens() {
        let out = render();
        assert_has(&out, "public struct Order: Sendable {");
        assert_has(&out, "w.writeObject(value.cart.clonePtr())");
        assert_has(&out, "Cart(ptr: r.readObject())");
    }

    #[test]
    fn object_iterator_adopts_each_element() {
        let out = render();
        assert_has(
            &out,
            "public final class ShopAllCartsIterator: Sequence, IteratorProtocol {",
        );
        assert_has(&out, "var item: OpaquePointer? = nil");
        assert_has(
            &out,
            "shop_kit_shop_AllCartsIterator_next(handle, &item, &err)",
        );
        assert_has(&out, "shop_kit_shop_AllCartsIterator_destroy(handle)");
        assert_has(&out, "return Cart(ptr: wvNonNull(item))");
    }

    #[test]
    fn callback_interface_renders_protocol_box_and_vtable() {
        let out = render();
        assert_has(&out, "public protocol Watcher: AnyObject, Sendable {");
        assert_has(
            &out,
            "func onEvent(name: String, count: Int32, order: Order, cart: Cart) throws -> Bool",
        );
        assert_has(&out, "final class WvWatcherBox: Sendable {");
        assert_has(
            &out,
            "static let shared = WvVtable<shop_kit_shop_Watcher_vtable>(shop_kit_shop_Watcher_vtable(",
        );
        assert_has(
            &out,
            "on_event: { ctx, name_ptr, name_len, count, order_ptr, order_len, cart, out_err in",
        );
        assert_has(&out, "name: wvBorrowString(name_ptr, name_len)");
        assert_has(
            &out,
            "order: wvBorrowBuffer(order_ptr, order_len, wvReadOrder)",
        );
        assert_has(&out, "cart: Cart(ptr: wvNonNull(cart))");
        assert_has(&out, "wvForeignError(out_err, error)");
        assert_has(&out, "Unmanaged<WvWatcherBox>.fromOpaque(ctx!).release()");
        assert_has(
            &out,
            "shop_kit_shop_watch(watcher_ctx, WvWatcherVtable.shared.pointer, &err)",
        );
    }

    #[test]
    fn cancellable_async_cancels_the_native_token() {
        let out = render();
        assert_has(
            &out,
            "public static func wait(ms: Int64) async throws -> Int64 {",
        );
        assert_has(&out, "let token = WvCancelToken()");
        assert_has(&out, "return try await withTaskCancellationHandler {");
        assert_has(
            &out,
            "shop_kit_shop_wait(ms, token.raw, { context, err, result in",
        );
        assert_has(
            &out,
            "cont.resume(throwing: wvTakeError(err, wvCancelledOrTrap))",
        );
        assert_has(&out, "} onCancel: {\n            token.cancel()");
    }
}
