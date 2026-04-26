use anyhow::Result;
use camino::Utf8Path;
use std::fmt::Write as _;
use weaveffi_core::codegen::{Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name, wrapper_name};
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, Function, ListenerDef, Module, StructDef, TypeRef,
};

pub struct AndroidGenerator;

impl AndroidGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        package: &str,
        strip_module_prefix: bool,
        c_prefix: &str,
    ) -> Result<()> {
        let dir = out_dir.join("android");
        std::fs::create_dir_all(&dir)?;

        std::fs::write(
            dir.join("settings.gradle"),
            "rootProject.name = 'weaveffi'\n",
        )?;
        std::fs::write(dir.join("build.gradle"), build_gradle(package))?;

        let pkg_path = package.replace('.', "/");
        let src_dir = dir.join(format!("src/main/kotlin/{pkg_path}"));
        std::fs::create_dir_all(&src_dir)?;
        let kotlin = render_kotlin(api, package, strip_module_prefix, c_prefix);
        std::fs::write(src_dir.join("WeaveFFI.kt"), kotlin)?;

        let jni_dir = dir.join("src/main/cpp");
        std::fs::create_dir_all(&jni_dir)?;
        std::fs::write(jni_dir.join("CMakeLists.txt"), cmake(c_prefix))?;
        let jni_c = render_jni_c(api, package, strip_module_prefix, c_prefix);
        std::fs::write(jni_dir.join("weaveffi_jni.c"), jni_c)?;

        Ok(())
    }
}

impl Generator for AndroidGenerator {
    fn name(&self) -> &'static str {
        "android"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "com.weaveffi", true, "weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(
            api,
            out_dir,
            config.android_package(),
            config.strip_module_prefix,
            config.c_prefix(),
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("android/settings.gradle").to_string(),
            out_dir.join("android/build.gradle").to_string(),
            out_dir
                .join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt")
                .to_string(),
            out_dir
                .join("android/src/main/cpp/CMakeLists.txt")
                .to_string(),
            out_dir
                .join("android/src/main/cpp/weaveffi_jni.c")
                .to_string(),
        ]
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::Callbacks,
            Capability::Listeners,
            Capability::Iterators,
            Capability::Builders,
            Capability::AsyncFunctions,
            Capability::CancellableAsync,
            Capability::TypedHandles,
            Capability::BorrowedTypes,
            Capability::MapTypes,
            Capability::NestedModules,
            Capability::CrossModuleTypes,
            Capability::ErrorDomains,
            Capability::DeprecatedAnnotations,
        ]
    }
}

fn build_gradle(namespace: &str) -> String {
    format!(
        r#"plugins {{
    id 'com.android.library'
    id 'org.jetbrains.kotlin.android' version '1.9.22' apply false
}}

android {{
    namespace '{namespace}'
    compileSdk 34
    defaultConfig {{
        minSdk 24
        externalNativeBuild {{
            cmake {{
                cppFlags ""
            }}
        }}
    }}
    externalNativeBuild {{
        cmake {{
            path "src/main/cpp/CMakeLists.txt"
        }}
    }}
}}
"#
    )
}

fn cmake(c_prefix: &str) -> String {
    format!(
        r#"cmake_minimum_required(VERSION 3.22)
project({c_prefix})
add_library({c_prefix} SHARED weaveffi_jni.c)
target_include_directories({c_prefix} PRIVATE ../../../../c)
"#
    )
}

fn kotlin_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I32 => "Int".to_string(),
        TypeRef::U32 => "Long".to_string(),
        TypeRef::I64 => "Long".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Boolean".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ByteArray".to_string(),
        TypeRef::Handle => "Long".to_string(),
        TypeRef::TypedHandle(name) => name.clone(),
        TypeRef::Struct(_) => "Long".to_string(),
        TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::Optional(inner) => format!("{}?", kotlin_type(inner)),
        TypeRef::List(inner) => kotlin_list_type(inner),
        TypeRef::Iterator(inner) => format!("Iterator<{}>", kotlin_type(inner)),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", kotlin_type(k), kotlin_type(v)),
        TypeRef::Callback(name) => name.clone(),
    }
}

fn kotlin_jni_type(t: &TypeRef) -> String {
    match t {
        TypeRef::TypedHandle(_) => "Long".to_string(),
        TypeRef::Callback(_) => "Long".to_string(),
        other => kotlin_type(other),
    }
}

fn kotlin_list_type(inner: &TypeRef) -> String {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => "IntArray".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => "LongArray".to_string(),
        TypeRef::F64 => "DoubleArray".to_string(),
        TypeRef::Bool => "BooleanArray".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Array<String>".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Array<ByteArray>".to_string(),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => {
            "LongArray".to_string()
        }
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback Android type")
        }
    }
}

fn jni_param_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I32 | TypeRef::Enum(_) => "jint".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Callback(_) => "jlong".to_string(),
        TypeRef::F64 => "jdouble".to_string(),
        TypeRef::Bool => "jboolean".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "jstring".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "jbyteArray".to_string(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "jstring".to_string(),
            TypeRef::Bytes | TypeRef::BorrowedBytes => "jbyteArray".to_string(),
            _ => "jobject".to_string(),
        },
        TypeRef::List(inner) | TypeRef::Iterator(inner) => jni_array_type(inner),
        TypeRef::Map(_, _) => "jobject".to_string(),
    }
}

fn jni_array_type(inner: &TypeRef) -> String {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => "jintArray".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => "jlongArray".to_string(),
        TypeRef::F64 => "jdoubleArray".to_string(),
        TypeRef::Bool => "jbooleanArray".to_string(),
        _ => "jobjectArray".to_string(),
    }
}

fn jni_ret_type(t: Option<&TypeRef>) -> String {
    match t {
        None => "void".to_string(),
        Some(t) => jni_param_type(t),
    }
}

fn c_type_for_return(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 | TypeRef::Enum(_) => "int32_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::I64 => "int64_t",
        TypeRef::F64 => "double",
        TypeRef::Bool => "bool",
        TypeRef::TypedHandle(_) | TypeRef::Handle => "weaveffi_handle_t",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*",
        TypeRef::Struct(_)
        | TypeRef::Optional(_)
        | TypeRef::List(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "void*",
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback Android type")
        }
    }
}

fn jni_default_return(t: Option<&TypeRef>) -> &'static str {
    match t {
        None => "",
        Some(TypeRef::I32 | TypeRef::Enum(_)) => "return 0;",
        Some(TypeRef::U32 | TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle) => {
            "return 0;"
        }
        Some(TypeRef::F64) => "return 0.0;",
        Some(TypeRef::Bool) => "return JNI_FALSE;",
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => "return NULL;",
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => "return NULL;",
        Some(TypeRef::Struct(_)) => "return 0;",
        Some(
            TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _),
        ) => "return NULL;",
        Some(TypeRef::Callback(_)) => {
            unreachable!("validator should have rejected callback Android type")
        }
    }
}

fn jni_cast_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 | TypeRef::Enum(_) => "(jint)",
        TypeRef::U32 | TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => "(jlong)",
        TypeRef::F64 => "(jdouble)",
        TypeRef::Struct(_) => "(jlong)(intptr_t)",
        _ => "",
    }
}

fn kotlin_public_type(t: &TypeRef) -> String {
    match t {
        TypeRef::Enum(name) => name.clone(),
        TypeRef::Callback(name) => name.clone(),
        other => kotlin_type(other),
    }
}

fn has_enum_involvement(f: &Function) -> bool {
    f.params
        .iter()
        .any(|p| matches!(&p.ty, TypeRef::Enum(_) | TypeRef::TypedHandle(_)))
        || matches!(&f.returns, Some(TypeRef::Enum(_)))
}

fn has_callback_params(f: &Function) -> bool {
    f.params
        .iter()
        .any(|p| matches!(&p.ty, TypeRef::Callback(_)))
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

fn collect_modules_with_path(modules: &[Module]) -> Vec<(&Module, String)> {
    let mut result = Vec::new();
    for m in modules {
        collect_module_with_path(m, &m.name, &mut result);
    }
    result
}

fn collect_module_with_path<'a>(m: &'a Module, path: &str, out: &mut Vec<(&'a Module, String)>) {
    out.push((m, path.to_string()));
    for sub in &m.modules {
        collect_module_with_path(sub, &format!("{path}_{}", sub.name), out);
    }
}

fn render_kotlin(api: &Api, package: &str, strip_module_prefix: bool, c_prefix: &str) -> String {
    let all_mods = collect_all_modules(&api.modules);
    let has_async = all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    let has_cancellable_async = all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));
    let mut kotlin = format!("package {package}\n\n");
    if has_async {
        kotlin.push_str("import kotlinx.coroutines.suspendCancellableCoroutine\n");
        kotlin.push_str("import kotlin.coroutines.resume\n");
        kotlin.push_str("import kotlin.coroutines.resumeWithException\n\n");
    }
    for m in &all_mods {
        for cb in &m.callbacks {
            render_kotlin_callback_typealias(&mut kotlin, cb);
        }
    }
    let _ = write!(
        kotlin,
        "class WeaveFFI {{\n    companion object {{\n        init {{ System.loadLibrary(\"{c_prefix}\") }}\n\n"
    );
    if has_cancellable_async {
        kotlin.push_str(
            "        @JvmStatic private external fun weaveffiCancelTokenCreate(): Long\n",
        );
        kotlin.push_str(
            "        @JvmStatic private external fun weaveffiCancelTokenCancel(token: Long)\n",
        );
        kotlin.push_str(
            "        @JvmStatic private external fun weaveffiCancelTokenDestroy(token: Long)\n",
        );
    }
    for m in &all_mods {
        for cb in &m.callbacks {
            let _ = writeln!(
                kotlin,
                "        @JvmStatic private external fun register{}(callback: {}): Long",
                cb.name, cb.name
            );
        }
    }
    for (m, path) in collect_modules_with_path(&api.modules) {
        for f in &m.functions {
            let func_name = wrapper_name(&path, &f.name, strip_module_prefix);
            if let Some(msg) = &f.deprecated {
                let _ = writeln!(
                    kotlin,
                    "        @Deprecated(\"{}\")",
                    msg.replace('"', "\\\"")
                );
            }
            if f.r#async {
                render_kotlin_async_fun(&mut kotlin, f, &func_name);
            } else if has_enum_involvement(f) || has_callback_params(f) {
                let native_params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, kotlin_jni_type(&p.ty)))
                    .collect();
                let native_ret = f
                    .returns
                    .as_ref()
                    .map(kotlin_jni_type)
                    .unwrap_or_else(|| "Unit".to_string());
                let _ = writeln!(
                    kotlin,
                    "        @JvmStatic private external fun {}Jni({}): {}",
                    func_name,
                    native_params.join(", "),
                    native_ret
                );

                let public_params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, kotlin_public_type(&p.ty)))
                    .collect();
                let public_ret = f
                    .returns
                    .as_ref()
                    .map(kotlin_public_type)
                    .unwrap_or_else(|| "Unit".to_string());
                let call_args: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| match &p.ty {
                        TypeRef::Enum(_) => format!("{}.value", p.name),
                        TypeRef::TypedHandle(_) => format!("{}.handle", p.name),
                        TypeRef::Callback(cb_name) => {
                            format!("register{}({})", cb_name, p.name)
                        }
                        _ => p.name.clone(),
                    })
                    .collect();
                let call = format!("{}Jni({})", func_name, call_args.join(", "));

                if let Some(TypeRef::Enum(name)) = &f.returns {
                    let _ = writeln!(
                        kotlin,
                        "        @JvmStatic fun {}({}): {} = {}.fromValue({})",
                        func_name,
                        public_params.join(", "),
                        public_ret,
                        name,
                        call
                    );
                } else if f.returns.is_some() {
                    let _ = writeln!(
                        kotlin,
                        "        @JvmStatic fun {}({}): {} = {}",
                        func_name,
                        public_params.join(", "),
                        public_ret,
                        call
                    );
                } else {
                    let _ = writeln!(
                        kotlin,
                        "        @JvmStatic fun {}({}) {{ {} }}",
                        func_name,
                        public_params.join(", "),
                        call
                    );
                }
            } else {
                let params_sig: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, kotlin_type(&p.ty)))
                    .collect();
                let ret = f
                    .returns
                    .as_ref()
                    .map(kotlin_type)
                    .unwrap_or_else(|| "Unit".to_string());
                let _ = writeln!(
                    kotlin,
                    "        @JvmStatic external fun {}({}): {}",
                    func_name,
                    params_sig.join(", "),
                    ret
                );
            }
        }
    }
    kotlin.push_str("    }\n}\n");
    for m in collect_all_modules(&api.modules) {
        for e in &m.enums {
            render_kotlin_enum(&mut kotlin, e);
        }
        for s in &m.structs {
            render_kotlin_struct(&mut kotlin, s, c_prefix);
            if s.builder {
                render_kotlin_builder(&mut kotlin, s);
            }
        }
    }
    for (m, _path) in collect_modules_with_path(&api.modules) {
        for l in &m.listeners {
            render_kotlin_listener(&mut kotlin, l, c_prefix);
        }
    }
    render_kotlin_error_types(&mut kotlin, api);
    if has_async {
        kotlin.push_str("\ninternal class WeaveContinuation<T>(private val cont: kotlinx.coroutines.CancellableContinuation<T>) {\n");
        kotlin.push_str("    @Suppress(\"UNCHECKED_CAST\")\n");
        kotlin.push_str("    fun onSuccess(result: Any?) { cont.resume(result as T) }\n");
        kotlin.push_str("    fun onError(message: String) { cont.resumeWithException(RuntimeException(message)) }\n");
        kotlin.push_str("}\n");
    }
    kotlin
}

fn render_kotlin_callback_typealias(out: &mut String, cb: &CallbackDef) {
    let params: Vec<String> = cb.params.iter().map(|p| kotlin_type(&p.ty)).collect();
    let ret = cb
        .returns
        .as_ref()
        .map(kotlin_type)
        .unwrap_or_else(|| "Unit".to_string());
    let _ = writeln!(
        out,
        "typealias {} = ({}) -> {}",
        cb.name,
        params.join(", "),
        ret
    );
}

/// Emit a Kotlin wrapper class for a listener. The companion object exposes
/// `register(callback): Long` and `unregister(id: Long)` as `@JvmStatic external`
/// functions; the JNI bridge reuses the shared callback registry to pin the
/// Kotlin lambda and installs the per-callback trampoline on the C ABI.
fn render_kotlin_listener(out: &mut String, l: &ListenerDef, c_prefix: &str) {
    let class_name = to_pascal_case(&l.name);
    let _ = writeln!(out);
    let _ = writeln!(out, "class {class_name} {{");
    let _ = writeln!(out, "    companion object {{");
    let _ = writeln!(out, "        init {{ System.loadLibrary(\"{c_prefix}\") }}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "        @JvmStatic external fun register(callback: {}): Long",
        l.event_callback
    );
    let _ = writeln!(out, "        @JvmStatic external fun unregister(id: Long)");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
}

fn render_kotlin_async_fun(out: &mut String, f: &Function, func_name: &str) {
    let mut native_param_chain: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, kotlin_type(&p.ty)))
        .collect();
    if f.cancellable {
        native_param_chain.push("cancelToken: Long".to_string());
    }
    native_param_chain.push("callback: Any".to_string());
    let native_params = native_param_chain;
    let _ = writeln!(
        out,
        "        @JvmStatic private external fun {}Async({})",
        func_name,
        native_params.join(", ")
    );

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, kotlin_type(&p.ty)))
        .collect();
    let ret = f
        .returns
        .as_ref()
        .map(kotlin_type)
        .unwrap_or_else(|| "Unit".to_string());
    let mut call_arg_chain: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
    if f.cancellable {
        call_arg_chain.push("token".to_string());
    }
    call_arg_chain.push("WeaveContinuation(cont)".to_string());
    let call_args = call_arg_chain;
    if let Some(msg) = &f.deprecated {
        let _ = writeln!(out, "        @Deprecated(\"{}\")", msg.replace('"', "\\\""));
    }
    if f.cancellable {
        let _ = writeln!(
            out,
            "        @JvmStatic suspend fun {}({}): {} {{",
            func_name,
            params_sig.join(", "),
            ret
        );
        let _ = writeln!(out, "            val token = weaveffiCancelTokenCreate()");
        let _ = writeln!(out, "            try {{");
        let _ = writeln!(
            out,
            "                return suspendCancellableCoroutine {{ cont ->"
        );
        let _ = writeln!(
            out,
            "                    cont.invokeOnCancellation {{ weaveffiCancelTokenCancel(token) }}"
        );
        let _ = writeln!(
            out,
            "                    {}Async({})",
            func_name,
            call_args.join(", ")
        );
        let _ = writeln!(out, "                }}");
        let _ = writeln!(out, "            }} finally {{");
        let _ = writeln!(out, "                weaveffiCancelTokenDestroy(token)");
        let _ = writeln!(out, "            }}");
        let _ = writeln!(out, "        }}");
    } else {
        let _ = writeln!(
            out,
            "        @JvmStatic suspend fun {}({}): {} = suspendCancellableCoroutine {{ cont ->",
            func_name,
            params_sig.join(", "),
            ret
        );
        let _ = writeln!(
            out,
            "            {}Async({})",
            func_name,
            call_args.join(", ")
        );
        let _ = writeln!(out, "        }}");
    }
}

fn render_kotlin_enum(out: &mut String, e: &EnumDef) {
    let _ = writeln!(out);
    let _ = writeln!(out, "enum class {}(val value: Int) {{", e.name);
    for (i, v) in e.variants.iter().enumerate() {
        let comma = if i < e.variants.len() - 1 { "," } else { ";" };
        let _ = writeln!(out, "    {}({}){}", v.name, v.value, comma);
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "    companion object {{");
    let _ = writeln!(
        out,
        "        fun fromValue(value: Int): {} = entries.first {{ it.value == value }}",
        e.name
    );
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
}

fn render_kotlin_error_types(out: &mut String, api: &Api) {
    let error_codes: Vec<_> = collect_all_modules(&api.modules)
        .iter()
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();

    let _ = writeln!(out);
    if error_codes.is_empty() {
        let _ = writeln!(
            out,
            "open class WeaveFFIException(val code: Int, message: String) : Exception(message)"
        );
    } else {
        let _ = writeln!(
            out,
            "sealed class WeaveFFIException(val code: Int, message: String) : Exception(message) {{"
        );
        for ec in &error_codes {
            let _ = writeln!(
                out,
                "    class {}(message: String = \"{}\") : WeaveFFIException({}, message)",
                ec.name, ec.message, ec.code
            );
        }
        let _ = writeln!(out, "}}");
    }
}

/// Emit JNI trampolines that expose the `weaveffi_cancel_token_*` C ABI to
/// Kotlin so cancellable `suspend fun`s can wire `invokeOnCancellation` to the
/// real native token instead of a null placeholder.
fn render_jni_cancel_token_helpers(out: &mut String, jni_prefix: &str) {
    let _ = writeln!(
        out,
        "JNIEXPORT jlong JNICALL Java_{jni_prefix}_WeaveFFI_weaveffiCancelTokenCreate(JNIEnv* env, jclass clazz) {{"
    );
    out.push_str("    (void)env; (void)clazz;\n");
    out.push_str("    return (jlong)(intptr_t)weaveffi_cancel_token_create();\n");
    out.push_str("}\n\n");

    let _ = writeln!(
        out,
        "JNIEXPORT void JNICALL Java_{jni_prefix}_WeaveFFI_weaveffiCancelTokenCancel(JNIEnv* env, jclass clazz, jlong token) {{"
    );
    out.push_str("    (void)env; (void)clazz;\n");
    out.push_str("    weaveffi_cancel_token_cancel((weaveffi_cancel_token*)(intptr_t)token);\n");
    out.push_str("}\n\n");

    let _ = writeln!(
        out,
        "JNIEXPORT void JNICALL Java_{jni_prefix}_WeaveFFI_weaveffiCancelTokenDestroy(JNIEnv* env, jclass clazz, jlong token) {{"
    );
    out.push_str("    (void)env; (void)clazz;\n");
    out.push_str("    weaveffi_cancel_token_destroy((weaveffi_cancel_token*)(intptr_t)token);\n");
    out.push_str("}\n\n");
}

fn render_jni_callback_registry(out: &mut String, _jni_prefix: &str) {
    out.push_str("static JavaVM* g_weaveffi_jvm = NULL;\n\n");
    out.push_str("typedef struct weaveffi_cb_entry {\n");
    out.push_str("    jlong id;\n");
    out.push_str("    jobject ref;\n");
    out.push_str("    struct weaveffi_cb_entry* next;\n");
    out.push_str("} weaveffi_cb_entry;\n\n");
    out.push_str("static weaveffi_cb_entry* g_weaveffi_cb_head = NULL;\n");
    out.push_str("static jlong g_weaveffi_cb_next_id = 1;\n");
    out.push_str("static pthread_mutex_t g_weaveffi_cb_mutex = PTHREAD_MUTEX_INITIALIZER;\n\n");
    out.push_str("static jobject weaveffi_cb_lookup(jlong id) {\n");
    out.push_str("    pthread_mutex_lock(&g_weaveffi_cb_mutex);\n");
    out.push_str("    jobject ref = NULL;\n");
    out.push_str("    for (weaveffi_cb_entry* e = g_weaveffi_cb_head; e != NULL; e = e->next) {\n");
    out.push_str("        if (e->id == id) { ref = e->ref; break; }\n");
    out.push_str("    }\n");
    out.push_str("    pthread_mutex_unlock(&g_weaveffi_cb_mutex);\n");
    out.push_str("    return ref;\n");
    out.push_str("}\n\n");
    out.push_str("static jlong weaveffi_cb_register(JNIEnv* env, jobject callback) {\n");
    out.push_str("    pthread_mutex_lock(&g_weaveffi_cb_mutex);\n");
    out.push_str("    if (g_weaveffi_jvm == NULL) { (*env)->GetJavaVM(env, &g_weaveffi_jvm); }\n");
    out.push_str(
        "    weaveffi_cb_entry* entry = (weaveffi_cb_entry*)malloc(sizeof(weaveffi_cb_entry));\n",
    );
    out.push_str("    entry->id = g_weaveffi_cb_next_id++;\n");
    out.push_str("    entry->ref = (*env)->NewGlobalRef(env, callback);\n");
    out.push_str("    entry->next = g_weaveffi_cb_head;\n");
    out.push_str("    g_weaveffi_cb_head = entry;\n");
    out.push_str("    jlong id = entry->id;\n");
    out.push_str("    pthread_mutex_unlock(&g_weaveffi_cb_mutex);\n");
    out.push_str("    return id;\n");
    out.push_str("}\n\n");
}

fn callback_c_return_type(ret: Option<&TypeRef>) -> &'static str {
    match ret {
        None => "void",
        Some(TypeRef::I32 | TypeRef::Enum(_)) => "int32_t",
        Some(TypeRef::U32) => "uint32_t",
        Some(TypeRef::I64) => "int64_t",
        Some(TypeRef::F64) => "double",
        Some(TypeRef::Bool) => "bool",
        Some(TypeRef::Handle | TypeRef::TypedHandle(_)) => "weaveffi_handle_t",
        Some(other) => unreachable!("unsupported callback return type: {other:?}"),
    }
}

fn callback_default_return(ret: Option<&TypeRef>) -> &'static str {
    match ret {
        None => "return;",
        Some(TypeRef::F64) => "return 0.0;",
        Some(TypeRef::Bool) => "return false;",
        _ => "return 0;",
    }
}

fn callback_trampoline_c_params(ty: &TypeRef, name: &str) -> Vec<String> {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => vec![format!("int32_t {name}")],
        TypeRef::U32 => vec![format!("uint32_t {name}")],
        TypeRef::I64 => vec![format!("int64_t {name}")],
        TypeRef::F64 => vec![format!("double {name}")],
        TypeRef::Bool => vec![format!("bool {name}")],
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            vec![format!("weaveffi_handle_t {name}")]
        }
        TypeRef::StringUtf8 => vec![
            format!("const uint8_t* {name}_ptr"),
            format!("size_t {name}_len"),
        ],
        TypeRef::BorrowedStr => vec![format!("const char* {name}")],
        other => unreachable!("unsupported callback parameter type: {other:?}"),
    }
}

fn write_callback_param_box(out: &mut String, ty: &TypeRef, name: &str) {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "    jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                n = name
            );
            let _ = writeln!(out, "    jmethodID {n}_vm = (*env)->GetStaticMethodID(env, {n}_cls, \"valueOf\", \"(I)Ljava/lang/Integer;\");", n = name);
            let _ = writeln!(out, "    jobject {n}_j = (*env)->CallStaticObjectMethod(env, {n}_cls, {n}_vm, (jint){n});", n = name);
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::TypedHandle(_) => {
            let _ = writeln!(
                out,
                "    jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                n = name
            );
            let _ = writeln!(out, "    jmethodID {n}_vm = (*env)->GetStaticMethodID(env, {n}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\");", n = name);
            let _ = writeln!(out, "    jobject {n}_j = (*env)->CallStaticObjectMethod(env, {n}_cls, {n}_vm, (jlong){n});", n = name);
        }
        TypeRef::F64 => {
            let _ = writeln!(
                out,
                "    jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                n = name
            );
            let _ = writeln!(out, "    jmethodID {n}_vm = (*env)->GetStaticMethodID(env, {n}_cls, \"valueOf\", \"(D)Ljava/lang/Double;\");", n = name);
            let _ = writeln!(out, "    jobject {n}_j = (*env)->CallStaticObjectMethod(env, {n}_cls, {n}_vm, (jdouble){n});", n = name);
        }
        TypeRef::Bool => {
            let _ = writeln!(
                out,
                "    jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                n = name
            );
            let _ = writeln!(out, "    jmethodID {n}_vm = (*env)->GetStaticMethodID(env, {n}_cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");", n = name);
            let _ = writeln!(out, "    jobject {n}_j = (*env)->CallStaticObjectMethod(env, {n}_cls, {n}_vm, {n} ? JNI_TRUE : JNI_FALSE);", n = name);
        }
        TypeRef::StringUtf8 => {
            let _ = writeln!(
                out,
                "    char* {n}_buf = (char*)malloc({n}_len + 1);",
                n = name
            );
            let _ = writeln!(
                out,
                "    if ({n}_ptr && {n}_len > 0) memcpy({n}_buf, {n}_ptr, {n}_len);",
                n = name
            );
            let _ = writeln!(out, "    {n}_buf[{n}_len] = 0;", n = name);
            let _ = writeln!(
                out,
                "    jobject {n}_j = (*env)->NewStringUTF(env, {n}_buf);",
                n = name
            );
            let _ = writeln!(out, "    free({n}_buf);", n = name);
        }
        TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "    jobject {n}_j = (*env)->NewStringUTF(env, {n} ? {n} : \"\");",
                n = name
            );
        }
        other => unreachable!("unsupported callback parameter type: {other:?}"),
    }
}

fn write_callback_return_unbox(out: &mut String, ty: &TypeRef) {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => {
            out.push_str("    jclass r_cls = (*env)->FindClass(env, \"java/lang/Integer\");\n");
            out.push_str(
                "    jmethodID r_mid = (*env)->GetMethodID(env, r_cls, \"intValue\", \"()I\");\n",
            );
            out.push_str(
                "    int32_t rv = (int32_t)(*env)->CallIntMethod(env, result_j, r_mid);\n",
            );
            out.push_str("    (*env)->DeleteLocalRef(env, result_j);\n");
            out.push_str("    return rv;\n");
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::TypedHandle(_) => {
            out.push_str("    jclass r_cls = (*env)->FindClass(env, \"java/lang/Long\");\n");
            out.push_str(
                "    jmethodID r_mid = (*env)->GetMethodID(env, r_cls, \"longValue\", \"()J\");\n",
            );
            let ret_ty = callback_c_return_type(Some(ty));
            let _ = writeln!(
                out,
                "    {ret_ty} rv = ({ret_ty})(*env)->CallLongMethod(env, result_j, r_mid);"
            );
            out.push_str("    (*env)->DeleteLocalRef(env, result_j);\n");
            out.push_str("    return rv;\n");
        }
        TypeRef::F64 => {
            out.push_str("    jclass r_cls = (*env)->FindClass(env, \"java/lang/Double\");\n");
            out.push_str("    jmethodID r_mid = (*env)->GetMethodID(env, r_cls, \"doubleValue\", \"()D\");\n");
            out.push_str(
                "    double rv = (double)(*env)->CallDoubleMethod(env, result_j, r_mid);\n",
            );
            out.push_str("    (*env)->DeleteLocalRef(env, result_j);\n");
            out.push_str("    return rv;\n");
        }
        TypeRef::Bool => {
            out.push_str("    jclass r_cls = (*env)->FindClass(env, \"java/lang/Boolean\");\n");
            out.push_str("    jmethodID r_mid = (*env)->GetMethodID(env, r_cls, \"booleanValue\", \"()Z\");\n");
            out.push_str("    jboolean rv = (*env)->CallBooleanMethod(env, result_j, r_mid);\n");
            out.push_str("    (*env)->DeleteLocalRef(env, result_j);\n");
            out.push_str("    return (bool)(rv == JNI_TRUE);\n");
        }
        other => unreachable!("unsupported callback return type: {other:?}"),
    }
}

fn render_jni_callback_trampoline(out: &mut String, module: &str, cb: &CallbackDef) {
    let trampoline_name = format!("weaveffi_{module}_{}_trampoline", cb.name);
    let ret_c = callback_c_return_type(cb.returns.as_ref());
    let default_ret = callback_default_return(cb.returns.as_ref());

    let mut c_params: Vec<String> = vec!["void* context".to_string()];
    for p in &cb.params {
        c_params.extend(callback_trampoline_c_params(&p.ty, &p.name));
    }

    let _ = writeln!(
        out,
        "static {ret_c} {trampoline_name}({}) {{",
        c_params.join(", ")
    );
    out.push_str("    jlong id = (jlong)(intptr_t)context;\n");
    out.push_str("    jobject lambda = weaveffi_cb_lookup(id);\n");
    let _ = writeln!(
        out,
        "    if (!lambda || !g_weaveffi_jvm) {{ {default_ret} }}"
    );
    out.push_str("    JNIEnv* env = NULL;\n");
    out.push_str(
        "    (*g_weaveffi_jvm)->AttachCurrentThread(g_weaveffi_jvm, (void**)&env, NULL);\n",
    );

    let sig_args = "Ljava/lang/Object;".repeat(cb.params.len());
    let sig = format!("({sig_args})Ljava/lang/Object;");
    out.push_str("    jclass lambda_cls = (*env)->GetObjectClass(env, lambda);\n");
    let _ = writeln!(
        out,
        "    jmethodID lambda_mid = (*env)->GetMethodID(env, lambda_cls, \"invoke\", \"{sig}\");"
    );

    for p in &cb.params {
        write_callback_param_box(out, &p.ty, &p.name);
    }
    let boxed_args: Vec<String> = cb.params.iter().map(|p| format!("{}_j", p.name)).collect();
    let call_args_suffix = if boxed_args.is_empty() {
        String::new()
    } else {
        format!(", {}", boxed_args.join(", "))
    };

    if cb.returns.is_none() {
        let _ = writeln!(
            out,
            "    (*env)->CallVoidMethod(env, lambda, lambda_mid{call_args_suffix});"
        );
        for p in &cb.params {
            let _ = writeln!(out, "    (*env)->DeleteLocalRef(env, {}_j);", p.name);
        }
        out.push_str("}\n\n");
    } else {
        let _ = writeln!(
            out,
            "    jobject result_j = (*env)->CallObjectMethod(env, lambda, lambda_mid{call_args_suffix});"
        );
        for p in &cb.params {
            let _ = writeln!(out, "    (*env)->DeleteLocalRef(env, {}_j);", p.name);
        }
        write_callback_return_unbox(out, cb.returns.as_ref().unwrap());
        out.push_str("}\n\n");
    }
}

fn render_jni_callback_register(out: &mut String, jni_prefix: &str, cb: &CallbackDef) {
    let _ = writeln!(
        out,
        "JNIEXPORT jlong JNICALL Java_{}_WeaveFFI_register{}(JNIEnv* env, jclass clazz, jobject callback) {{",
        jni_prefix, cb.name
    );
    out.push_str("    return weaveffi_cb_register(env, callback);\n");
    out.push_str("}\n\n");
}

fn render_jni_listener(out: &mut String, module: &str, l: &ListenerDef, jni_prefix: &str) {
    let class_name = to_pascal_case(&l.name);
    let trampoline = format!("weaveffi_{module}_{}_trampoline", l.event_callback);
    let reg_sym = format!("weaveffi_{module}_register_{}", l.name);
    let unreg_sym = format!("weaveffi_{module}_unregister_{}", l.name);

    let _ = writeln!(
        out,
        "JNIEXPORT jlong JNICALL Java_{jni_prefix}_{class_name}_register(JNIEnv* env, jclass clazz, jobject callback) {{"
    );
    out.push_str("    jlong token = weaveffi_cb_register(env, callback);\n");
    let _ = writeln!(
        out,
        "    uint64_t id = {reg_sym}(&{trampoline}, (void*)(intptr_t)token);"
    );
    out.push_str("    return (jlong)id;\n");
    out.push_str("}\n\n");

    let _ = writeln!(
        out,
        "JNIEXPORT void JNICALL Java_{jni_prefix}_{class_name}_unregister(JNIEnv* env, jclass clazz, jlong id) {{"
    );
    out.push_str("    (void)env; (void)clazz;\n");
    let _ = writeln!(out, "    {unreg_sym}((uint64_t)id);");
    out.push_str("}\n\n");
}

fn render_jni_c(api: &Api, package: &str, strip_module_prefix: bool, c_prefix: &str) -> String {
    let jni_prefix = package.replace('.', "_");
    let jni_pkg_path = package.replace('.', "/");
    let mut jni_c = format!("#include <jni.h>\n#include <stdbool.h>\n#include <stdint.h>\n#include <stddef.h>\n#include <stdlib.h>\n#include <string.h>\n#include <pthread.h>\n#include \"{c_prefix}.h\"\n\n");

    let all_mods = collect_all_modules(&api.modules);
    let error_codes: Vec<_> = all_mods
        .iter()
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();

    jni_c.push_str("static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {\n");
    jni_c.push_str("    const char* msg = err->message ? err->message : \"WeaveFFI error\";\n");
    if error_codes.is_empty() {
        jni_c.push_str(
            "    jclass exClass = (*env)->FindClass(env, \"java/lang/RuntimeException\");\n",
        );
    } else {
        jni_c.push_str("    jclass exClass;\n");
        jni_c.push_str("    switch (err->code) {\n");
        for ec in &error_codes {
            let _ = writeln!(jni_c, "    case {}:", ec.code);
            let _ = writeln!(
                jni_c,
                "        exClass = (*env)->FindClass(env, \"{}/WeaveFFIException${}\");",
                jni_pkg_path, ec.name
            );
            jni_c.push_str("        break;\n");
        }
        jni_c.push_str("    default:\n");
        jni_c.push_str(
            "        exClass = (*env)->FindClass(env, \"java/lang/RuntimeException\");\n",
        );
        jni_c.push_str("        break;\n");
        jni_c.push_str("    }\n");
    }
    jni_c.push_str("    (*env)->ThrowNew(env, exClass, msg);\n");
    jni_c.push_str("    weaveffi_error_clear(err);\n");
    jni_c.push_str("}\n\n");

    let has_async = all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    if has_async {
        jni_c.push_str("typedef struct {\n");
        jni_c.push_str("    JavaVM* jvm;\n");
        jni_c.push_str("    jobject callback;\n");
        jni_c.push_str("} weaveffi_jni_async_ctx;\n\n");
    }

    let has_cancellable_async = all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));
    if has_cancellable_async {
        render_jni_cancel_token_helpers(&mut jni_c, &jni_prefix);
    }

    let has_callbacks = all_mods.iter().any(|m| !m.callbacks.is_empty());
    if has_callbacks {
        render_jni_callback_registry(&mut jni_c, &jni_prefix);
        for (m, path) in collect_modules_with_path(&api.modules) {
            for cb in &m.callbacks {
                render_jni_callback_trampoline(&mut jni_c, &path, cb);
                render_jni_callback_register(&mut jni_c, &jni_prefix, cb);
            }
        }
    }

    for (m, path) in collect_modules_with_path(&api.modules) {
        for l in &m.listeners {
            render_jni_listener(&mut jni_c, &path, l, &jni_prefix);
        }
    }

    for (m, path) in collect_modules_with_path(&api.modules) {
        for f in &m.functions {
            if f.r#async {
                let func_name = wrapper_name(&path, &f.name, strip_module_prefix);
                render_jni_async_function(&mut jni_c, &path, f, &func_name, &jni_prefix);
                continue;
            }
            let jret = jni_ret_type(f.returns.as_ref());
            let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
            for p in &f.params {
                jparams.push(format!("{} {}", jni_param_type(&p.ty), p.name));
            }
            let func_name = wrapper_name(&path, &f.name, strip_module_prefix);
            let jni_name = if has_enum_involvement(f) || has_callback_params(f) {
                format!("{}Jni", func_name)
            } else {
                func_name
            };
            let _ = writeln!(
                jni_c,
                "JNIEXPORT {} JNICALL Java_{}_WeaveFFI_{}({}) {{",
                jret,
                jni_prefix,
                jni_name,
                jparams.join(", ")
            );
            let _ = writeln!(jni_c, "    weaveffi_error err = {{0, NULL}};");

            for p in &f.params {
                write_param_acquire(&mut jni_c, &p.name, &p.ty);
            }

            let c_sym = c_symbol_name(&path, &f.name);
            let mut call_args: Vec<String> = Vec::new();
            for p in &f.params {
                build_c_call_args(&mut call_args, &p.name, &p.ty, &path);
            }

            let needs_out_len = matches!(
                f.returns,
                Some(TypeRef::Bytes | TypeRef::BorrowedBytes) | Some(TypeRef::List(_))
            );
            if needs_out_len {
                let _ = writeln!(jni_c, "    size_t out_len = 0;");
            }

            if let Some(ret_type) = f.returns.as_ref() {
                write_return_handling(
                    &mut jni_c,
                    ret_type,
                    &c_sym,
                    &call_args,
                    f.returns.as_ref(),
                    &f.params,
                    &path,
                );
            } else {
                let _ = writeln!(jni_c, "    {}({}, &err);", c_sym, call_args.join(", "));
                write_error_check(&mut jni_c, f.returns.as_ref());
                release_jni_resources(&mut jni_c, &f.params);
                let _ = writeln!(jni_c, "    return;");
            }

            let _ = writeln!(jni_c, "}}\n");
        }
    }
    for (m, path) in collect_modules_with_path(&api.modules) {
        for s in &m.structs {
            render_jni_struct(&mut jni_c, &path, s, &jni_prefix);
        }
    }
    jni_c
}

fn async_cb_result_params(ret: Option<&TypeRef>) -> String {
    match ret {
        None => String::new(),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => ", const char* result".to_string(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            ", const uint8_t* result, size_t result_len".to_string()
        }
        Some(t) => format!(", {} result", c_type_for_return(t)),
    }
}

fn write_jni_box_result(out: &mut String, ret: Option<&TypeRef>) {
    match ret {
        None => {
            out.push_str("        jobject boxed = NULL;\n");
        }
        Some(TypeRef::I32 | TypeRef::Enum(_)) => {
            out.push_str(
                "        jclass boxCls = (*env)->FindClass(env, \"java/lang/Integer\");\n",
            );
            out.push_str("        jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(I)Ljava/lang/Integer;\");\n");
            out.push_str("        jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jint)result);\n");
        }
        Some(
            TypeRef::U32
            | TypeRef::I64
            | TypeRef::Handle
            | TypeRef::TypedHandle(_)
            | TypeRef::Struct(_),
        ) => {
            out.push_str("        jclass boxCls = (*env)->FindClass(env, \"java/lang/Long\");\n");
            out.push_str("        jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(J)Ljava/lang/Long;\");\n");
            out.push_str("        jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jlong)result);\n");
        }
        Some(TypeRef::F64) => {
            out.push_str("        jclass boxCls = (*env)->FindClass(env, \"java/lang/Double\");\n");
            out.push_str("        jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(D)Ljava/lang/Double;\");\n");
            out.push_str("        jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jdouble)result);\n");
        }
        Some(TypeRef::Bool) => {
            out.push_str(
                "        jclass boxCls = (*env)->FindClass(env, \"java/lang/Boolean\");\n",
            );
            out.push_str("        jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");\n");
            out.push_str("        jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, result ? JNI_TRUE : JNI_FALSE);\n");
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str("        jobject boxed = (*env)->NewStringUTF(env, result);\n");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(
                "        jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);\n",
            );
            out.push_str("        if (boxed && result) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result); }\n");
            out.push_str("        weaveffi_free_bytes((uint8_t*)result, (size_t)result_len);\n");
        }
        _ => {
            out.push_str("        jobject boxed = (jobject)(intptr_t)result;\n");
        }
    }
    out.push_str("        jclass cls = (*env)->GetObjectClass(env, ctx->callback);\n");
    out.push_str("        jmethodID mid = (*env)->GetMethodID(env, cls, \"onSuccess\", \"(Ljava/lang/Object;)V\");\n");
    out.push_str("        (*env)->CallVoidMethod(env, ctx->callback, mid, boxed);\n");
}

fn render_jni_async_function(
    out: &mut String,
    module_name: &str,
    f: &Function,
    func_name: &str,
    jni_prefix: &str,
) {
    let c_sym = c_symbol_name(module_name, &f.name);
    let cb_name = format!("{c_sym}_jni_cb");
    let cb_result_params = async_cb_result_params(f.returns.as_ref());

    let _ = writeln!(
        out,
        "static void {cb_name}(void* context, weaveffi_error* err{cb_result_params}) {{"
    );
    out.push_str("    weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)context;\n");
    out.push_str("    JNIEnv* env;\n");
    out.push_str("    (*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL);\n");
    out.push_str("    if (err != NULL && err->code != 0) {\n");
    out.push_str("        const char* msg = err->message ? err->message : \"WeaveFFI error\";\n");
    out.push_str("        jstring jmsg = (*env)->NewStringUTF(env, msg);\n");
    out.push_str("        jclass cls = (*env)->GetObjectClass(env, ctx->callback);\n");
    out.push_str("        jmethodID mid = (*env)->GetMethodID(env, cls, \"onError\", \"(Ljava/lang/String;)V\");\n");
    out.push_str("        (*env)->CallVoidMethod(env, ctx->callback, mid, jmsg);\n");
    out.push_str("    } else {\n");
    write_jni_box_result(out, f.returns.as_ref());
    out.push_str("    }\n");
    out.push_str("    (*env)->DeleteGlobalRef(env, ctx->callback);\n");
    out.push_str("    free(ctx);\n");
    out.push_str("}\n\n");

    let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
    for p in &f.params {
        jparams.push(format!("{} {}", jni_param_type(&p.ty), p.name));
    }
    if f.cancellable {
        jparams.push("jlong cancelToken".to_string());
    }
    jparams.push("jobject callback".to_string());

    let jni_name = format!("{func_name}Async");
    let _ = writeln!(
        out,
        "JNIEXPORT void JNICALL Java_{}_WeaveFFI_{}({}) {{",
        jni_prefix,
        jni_name,
        jparams.join(", ")
    );

    out.push_str("    weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)malloc(sizeof(weaveffi_jni_async_ctx));\n");
    out.push_str("    (*env)->GetJavaVM(env, &ctx->jvm);\n");
    out.push_str("    ctx->callback = (*env)->NewGlobalRef(env, callback);\n");

    for p in &f.params {
        write_param_acquire(out, &p.name, &p.ty);
    }

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        build_c_call_args(&mut call_args, &p.name, &p.ty, module_name);
    }
    if f.cancellable {
        call_args.push("(weaveffi_cancel_token*)(intptr_t)cancelToken".to_string());
    }
    call_args.push(cb_name);
    call_args.push("ctx".to_string());

    let _ = writeln!(out, "    {c_sym}_async({});", call_args.join(", "));

    release_jni_resources(out, &f.params);

    out.push_str("}\n\n");
}

fn write_param_acquire(out: &mut String, name: &str, ty: &TypeRef) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "    const char* {n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetStringUTFLength(env, {n});",
                n = name
            );
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(out, "    jboolean {n}_is_copy = 0;", n = name);
            let _ = writeln!(
                out,
                "    jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, &{n}_is_copy);",
                n = name
            );
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
        }
        TypeRef::Optional(inner) => write_optional_acquire(out, name, inner),
        TypeRef::List(inner) => write_list_acquire(out, name, inner),
        TypeRef::Map(k, v) => write_map_acquire(out, name, k, v),
        _ => {}
    }
}

fn write_optional_acquire(out: &mut String, name: &str, inner: &TypeRef) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(out, "    const char* {n}_chars = NULL;", n = name);
            let _ = writeln!(out, "    jsize {n}_len = 0;", n = name);
            let _ = writeln!(out, "    if ({n} != NULL) {{", n = name);
            let _ = writeln!(
                out,
                "        {n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "        {n}_len = (*env)->GetStringUTFLength(env, {n});",
                n = name
            );
            let _ = writeln!(out, "    }}");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(out, "    jbyte* {n}_elems = NULL;", n = name);
            let _ = writeln!(out, "    jsize {n}_len = 0;", n = name);
            let _ = writeln!(out, "    if ({n} != NULL) {{", n = name);
            let _ = writeln!(
                out,
                "        {n}_elems = (*env)->GetByteArrayElements(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "        {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
            let _ = writeln!(out, "    }}");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(out, "    int32_t {n}_val = 0;", n = name);
            let _ = writeln!(out, "    const int32_t* {n}_ptr = NULL;", n = name);
            let _ = writeln!(out, "    if ({n} != NULL) {{", n = name);
            let _ = writeln!(
                out,
                "        jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"intValue\", \"()I\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        {n}_val = (int32_t)(*env)->CallIntMethod(env, {n}, {n}_mid);",
                n = name
            );
            let _ = writeln!(out, "        {n}_ptr = &{n}_val;", n = name);
            let _ = writeln!(out, "    }}");
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            let _ = writeln!(out, "    int64_t {n}_val = 0;", n = name);
            let _ = writeln!(out, "    const int64_t* {n}_ptr = NULL;", n = name);
            let _ = writeln!(out, "    if ({n} != NULL) {{", n = name);
            let _ = writeln!(
                out,
                "        jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"longValue\", \"()J\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        {n}_val = (int64_t)(*env)->CallLongMethod(env, {n}, {n}_mid);",
                n = name
            );
            let _ = writeln!(out, "        {n}_ptr = &{n}_val;", n = name);
            let _ = writeln!(out, "    }}");
        }
        TypeRef::F64 => {
            let _ = writeln!(out, "    double {n}_val = 0.0;", n = name);
            let _ = writeln!(out, "    const double* {n}_ptr = NULL;", n = name);
            let _ = writeln!(out, "    if ({n} != NULL) {{", n = name);
            let _ = writeln!(
                out,
                "        jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"doubleValue\", \"()D\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        {n}_val = (*env)->CallDoubleMethod(env, {n}, {n}_mid);",
                n = name
            );
            let _ = writeln!(out, "        {n}_ptr = &{n}_val;", n = name);
            let _ = writeln!(out, "    }}");
        }
        TypeRef::Bool => {
            let _ = writeln!(out, "    bool {n}_val = false;", n = name);
            let _ = writeln!(out, "    const bool* {n}_ptr = NULL;", n = name);
            let _ = writeln!(out, "    if ({n} != NULL) {{", n = name);
            let _ = writeln!(
                out,
                "        jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"booleanValue\", \"()Z\");",
                n = name
            );
            let _ = writeln!(
                out,
                "        {n}_val = (bool)(*env)->CallBooleanMethod(env, {n}, {n}_mid);",
                n = name
            );
            let _ = writeln!(out, "        {n}_ptr = &{n}_val;", n = name);
            let _ = writeln!(out, "    }}");
        }
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => {}
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback Android type")
        }
    }
}

fn write_list_acquire(out: &mut String, name: &str, inner: &TypeRef) {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "    jint* {n}_elems = (*env)->GetIntArrayElements(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            let _ = writeln!(
                out,
                "    jlong* {n}_elems = (*env)->GetLongArrayElements(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
        }
        TypeRef::F64 => {
            let _ = writeln!(
                out,
                "    jdouble* {n}_elems = (*env)->GetDoubleArrayElements(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
        }
        TypeRef::Bool => {
            let _ = writeln!(
                out,
                "    jboolean* {n}_elems = (*env)->GetBooleanArrayElements(env, {n}, NULL);",
                n = name
            );
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
        }
        _ => {
            let _ = writeln!(
                out,
                "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            );
        }
    }
}

fn map_elem_c_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => "int32_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => "int64_t",
        TypeRef::F64 => "double",
        TypeRef::Bool => "jboolean",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*",
        _ => "void*",
    }
}

fn map_elem_c_call_cast(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => "(const int32_t*)",
        TypeRef::U32 => "(const uint32_t*)",
        TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => "(const int64_t*)",
        TypeRef::F64 => "(const double*)",
        TypeRef::Bool => "(const bool*)",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "(const char* const*)",
        _ => "(const void*)",
    }
}

fn write_map_acquire(out: &mut String, name: &str, key: &TypeRef, val: &TypeRef) {
    let key_c = map_elem_c_type(key);
    let val_c = map_elem_c_type(val);
    let _ = writeln!(
        out,
        "    jclass {n}_mc = (*env)->FindClass(env, \"java/util/Map\");",
        n = name
    );
    let _ = writeln!(
        out,
        "    jsize {n}_len = (jsize)(*env)->CallIntMethod(env, {n}, (*env)->GetMethodID(env, {n}_mc, \"size\", \"()I\"));",
        n = name
    );
    let _ = writeln!(
        out,
        "    jobject {n}_ks = (*env)->CallObjectMethod(env, {n}, (*env)->GetMethodID(env, {n}_mc, \"keySet\", \"()Ljava/util/Set;\"));",
        n = name
    );
    let _ = writeln!(
        out,
        "    jclass {n}_sc = (*env)->FindClass(env, \"java/util/Set\");",
        n = name
    );
    let _ = writeln!(
        out,
        "    jobjectArray {n}_ka = (jobjectArray)(*env)->CallObjectMethod(env, {n}_ks, (*env)->GetMethodID(env, {n}_sc, \"toArray\", \"()[Ljava/lang/Object;\"));",
        n = name
    );
    let _ = writeln!(
        out,
        "    jmethodID {n}_gm = (*env)->GetMethodID(env, {n}_mc, \"get\", \"(Ljava/lang/Object;)Ljava/lang/Object;\");",
        n = name
    );
    let _ = writeln!(
        out,
        "    {kc}* {n}_c_keys = ({kc}*)malloc((size_t){n}_len * sizeof({kc}));",
        kc = key_c,
        n = name
    );
    let _ = writeln!(
        out,
        "    {vc}* {n}_c_vals = ({vc}*)malloc((size_t){n}_len * sizeof({vc}));",
        vc = val_c,
        n = name
    );
    if matches!(key, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let _ = writeln!(
            out,
            "    jstring* {n}_jk = (jstring*)malloc((size_t){n}_len * sizeof(jstring));",
            n = name
        );
    }
    if matches!(val, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let _ = writeln!(
            out,
            "    jstring* {n}_jv = (jstring*)malloc((size_t){n}_len * sizeof(jstring));",
            n = name
        );
    }
    write_map_unbox_setup(out, name, "k", key);
    write_map_unbox_setup(out, name, "v", val);
    let _ = writeln!(
        out,
        "    for (jsize {n}_i = 0; {n}_i < {n}_len; {n}_i++) {{",
        n = name
    );
    let _ = writeln!(
        out,
        "        jobject {n}_ko = (*env)->GetObjectArrayElement(env, {n}_ka, {n}_i);",
        n = name
    );
    write_map_elem_extract(out, name, "k", "c_keys", key, &format!("{name}_ko"));
    let _ = writeln!(
        out,
        "        jobject {n}_vo = (*env)->CallObjectMethod(env, {n}, {n}_gm, {n}_ko);",
        n = name
    );
    write_map_elem_extract(out, name, "v", "c_vals", val, &format!("{name}_vo"));
    let _ = writeln!(out, "    }}");
}

fn write_map_unbox_setup(out: &mut String, name: &str, suffix: &str, ty: &TypeRef) {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "    jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Integer\");",
                n = name,
                s = suffix
            );
            let _ = writeln!(
                out,
                "    jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"intValue\", \"()I\");",
                n = name,
                s = suffix
            );
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            let _ = writeln!(
                out,
                "    jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Long\");",
                n = name,
                s = suffix
            );
            let _ = writeln!(
                out,
                "    jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"longValue\", \"()J\");",
                n = name,
                s = suffix
            );
        }
        TypeRef::F64 => {
            let _ = writeln!(
                out,
                "    jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Double\");",
                n = name,
                s = suffix
            );
            let _ = writeln!(
                out,
                "    jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"doubleValue\", \"()D\");",
                n = name,
                s = suffix
            );
        }
        TypeRef::Bool => {
            let _ = writeln!(
                out,
                "    jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Boolean\");",
                n = name,
                s = suffix
            );
            let _ = writeln!(
                out,
                "    jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"booleanValue\", \"()Z\");",
                n = name,
                s = suffix
            );
        }
        _ => {}
    }
}

fn write_map_elem_extract(
    out: &mut String,
    name: &str,
    suffix: &str,
    arr: &str,
    ty: &TypeRef,
    obj_var: &str,
) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "        {n}_j{s}[{n}_i] = (jstring){obj};",
                n = name,
                s = suffix,
                obj = obj_var
            );
            let _ = writeln!(
                out,
                "        {n}_{a}[{n}_i] = (*env)->GetStringUTFChars(env, (jstring){obj}, NULL);",
                n = name,
                a = arr,
                obj = obj_var
            );
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "        {n}_{a}[{n}_i] = (int32_t)(*env)->CallIntMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            );
        }
        TypeRef::U32 => {
            let _ = writeln!(
                out,
                "        {n}_{a}[{n}_i] = (uint32_t)(*env)->CallLongMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            );
        }
        TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            let _ = writeln!(
                out,
                "        {n}_{a}[{n}_i] = (int64_t)(*env)->CallLongMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            );
        }
        TypeRef::F64 => {
            let _ = writeln!(
                out,
                "        {n}_{a}[{n}_i] = (*env)->CallDoubleMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            );
        }
        TypeRef::Bool => {
            let _ = writeln!(
                out,
                "        {n}_{a}[{n}_i] = (*env)->CallBooleanMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            );
        }
        _ => {}
    }
}

fn build_c_call_args(args: &mut Vec<String>, name: &str, ty: &TypeRef, module: &str) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            args.push(format!("(const uint8_t*){n}_chars", n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            args.push(format!("(const uint8_t*){n}_elems", n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Bool => args.push(format!("(bool)({} == JNI_TRUE)", name)),
        TypeRef::I32 => args.push(format!("(int32_t){}", name)),
        TypeRef::U32 => args.push(format!("(uint32_t){}", name)),
        TypeRef::I64 => args.push(format!("(int64_t){}", name)),
        TypeRef::F64 => args.push(format!("(double){}", name)),
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            args.push(format!("(weaveffi_handle_t){}", name))
        }
        TypeRef::Struct(sname) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(sname, module, "weaveffi");
            args.push(format!("(const {}*)(intptr_t){}", c_struct, name));
        }
        TypeRef::Enum(_) => args.push(format!("(int32_t){}", name)),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                args.push(format!("(const uint8_t*){n}_chars", n = name));
                args.push(format!("(size_t){n}_len", n = name));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                args.push(format!("(const uint8_t*){n}_elems", n = name));
                args.push(format!("(size_t){n}_len", n = name));
            }
            _ => args.push(format!("{}_ptr", name)),
        },
        TypeRef::List(inner) => {
            match inner.as_ref() {
                TypeRef::I32 | TypeRef::Enum(_) => {
                    args.push(format!("(const int32_t*){n}_elems", n = name));
                }
                TypeRef::U32 => {
                    args.push(format!("(const uint32_t*){n}_elems", n = name));
                }
                TypeRef::I64 => {
                    args.push(format!("(const int64_t*){n}_elems", n = name));
                }
                TypeRef::F64 => {
                    args.push(format!("(const double*){n}_elems", n = name));
                }
                TypeRef::Bool => {
                    args.push(format!("(const bool*){n}_elems", n = name));
                }
                TypeRef::TypedHandle(_) | TypeRef::Handle => {
                    args.push(format!("(const weaveffi_handle_t*){n}_elems", n = name));
                }
                _ => {
                    args.push(format!("(const void*){n}_elems", n = name));
                }
            }
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Map(k, v) => {
            args.push(format!("{}{n}_c_keys", map_elem_c_call_cast(k), n = name));
            args.push(format!("{}{n}_c_vals", map_elem_c_call_cast(v), n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Callback(cb_name) => {
            args.push(format!("&weaveffi_{module}_{cb_name}_trampoline"));
            args.push(format!("(void*)(intptr_t){name}"));
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

#[allow(clippy::too_many_arguments)]
fn write_return_handling(
    jni_c: &mut String,
    ret_type: &TypeRef,
    c_sym: &str,
    call_args: &[String],
    returns: Option<&TypeRef>,
    params: &[weaveffi_ir::ir::Param],
    module: &str,
) {
    let args_str = call_args.join(", ");
    match ret_type {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(jni_c, "    const char* rv = {}({}, &err);", c_sym, args_str);
            write_error_check(jni_c, returns);
            let _ = writeln!(jni_c, "    jstring out = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");");
            let _ = writeln!(jni_c, "    weaveffi_free_string(rv);");
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return out;");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(
                jni_c,
                "    uint8_t* rv = {}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(jni_c, returns);
            let _ = writeln!(
                jni_c,
                "    jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"
            );
            let _ = writeln!(jni_c, "    if (out && rv) {{ (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }}");
            let _ = writeln!(jni_c, "    weaveffi_free_bytes(rv, (size_t)out_len);");
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return out;");
        }
        TypeRef::Bool => {
            let _ = writeln!(jni_c, "    bool rv = {}({}, &err);", c_sym, args_str);
            write_error_check(jni_c, returns);
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return rv ? JNI_TRUE : JNI_FALSE;");
        }
        TypeRef::Struct(name) => {
            let c_ty = weaveffi_core::utils::c_abi_struct_name(name, module, "weaveffi");
            let _ = writeln!(jni_c, "    {}* rv = {}({}, &err);", c_ty, c_sym, args_str);
            write_error_check(jni_c, returns);
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return (jlong)(intptr_t)rv;");
        }
        TypeRef::Optional(inner) => {
            write_optional_return(jni_c, inner, c_sym, &args_str, returns, params, module);
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            write_list_return(jni_c, inner, c_sym, &args_str, returns, params);
        }
        TypeRef::Map(k, v) => {
            write_map_return(jni_c, k, v, c_sym, &args_str, returns, params);
        }
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback Android type")
        }
        ret_type => {
            let c_ty = c_type_for_return(ret_type);
            let jcast = jni_cast_for(ret_type);
            let _ = writeln!(jni_c, "    {} rv = {}({}, &err);", c_ty, c_sym, args_str);
            write_error_check(jni_c, returns);
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return {} rv;", jcast);
        }
    }
}

fn write_optional_return(
    out: &mut String,
    inner: &TypeRef,
    c_sym: &str,
    args_str: &str,
    returns: Option<&TypeRef>,
    params: &[weaveffi_ir::ir::Param],
    _module: &str,
) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(out, "    const char* rv = {}({}, &err);", c_sym, args_str);
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(out, "    jstring result = (*env)->NewStringUTF(env, rv);");
            let _ = writeln!(out, "    weaveffi_free_string(rv);");
            let _ = writeln!(out, "    return result;");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "    const int32_t* rv = {}({}, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(
                out,
                "    jclass cls = (*env)->FindClass(env, \"java/lang/Integer\");"
            );
            let _ = writeln!(
                out,
                "    jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(I)Ljava/lang/Integer;\");"
            );
            let _ = writeln!(
                out,
                "    return (*env)->CallStaticObjectMethod(env, cls, mid, (jint)*rv);"
            );
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            let _ = writeln!(
                out,
                "    const int64_t* rv = (const int64_t*){}({}, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(
                out,
                "    jclass cls = (*env)->FindClass(env, \"java/lang/Long\");"
            );
            let _ = writeln!(
                out,
                "    jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(J)Ljava/lang/Long;\");"
            );
            let _ = writeln!(
                out,
                "    return (*env)->CallStaticObjectMethod(env, cls, mid, (jlong)*rv);"
            );
        }
        TypeRef::F64 => {
            let _ = writeln!(out, "    const double* rv = {}({}, &err);", c_sym, args_str);
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(
                out,
                "    jclass cls = (*env)->FindClass(env, \"java/lang/Double\");"
            );
            let _ = writeln!(
                out,
                "    jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(D)Ljava/lang/Double;\");"
            );
            let _ = writeln!(
                out,
                "    return (*env)->CallStaticObjectMethod(env, cls, mid, (jdouble)*rv);"
            );
        }
        TypeRef::Bool => {
            let _ = writeln!(out, "    const bool* rv = {}({}, &err);", c_sym, args_str);
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(
                out,
                "    jclass cls = (*env)->FindClass(env, \"java/lang/Boolean\");"
            );
            let _ = writeln!(
                out,
                "    jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");"
            );
            let _ = writeln!(
                out,
                "    return (*env)->CallStaticObjectMethod(env, cls, mid, *rv ? JNI_TRUE : JNI_FALSE);"
            );
        }
        _ => {
            let _ = writeln!(out, "    void* rv = {}({}, &err);", c_sym, args_str);
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    return (jobject)rv;");
        }
    }
}

fn write_list_return(
    out: &mut String,
    inner: &TypeRef,
    c_sym: &str,
    args_str: &str,
    returns: Option<&TypeRef>,
    params: &[weaveffi_ir::ir::Param],
) {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "    const int32_t* rv = {}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(
                out,
                "    jintArray result = (*env)->NewIntArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (result && rv) {{ (*env)->SetIntArrayRegion(env, result, 0, (jsize)out_len, (const jint*)rv); }}");
            let _ = writeln!(out, "    return result;");
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            let _ = writeln!(
                out,
                "    const int64_t* rv = (const int64_t*){}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(
                out,
                "    jlongArray result = (*env)->NewLongArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (result && rv) {{ (*env)->SetLongArrayRegion(env, result, 0, (jsize)out_len, (const jlong*)rv); }}");
            let _ = writeln!(out, "    return result;");
        }
        TypeRef::F64 => {
            let _ = writeln!(
                out,
                "    const double* rv = {}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(
                out,
                "    jdoubleArray result = (*env)->NewDoubleArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (result && rv) {{ (*env)->SetDoubleArrayRegion(env, result, 0, (jsize)out_len, (const jdouble*)rv); }}");
            let _ = writeln!(out, "    return result;");
        }
        TypeRef::Bool => {
            let _ = writeln!(
                out,
                "    const bool* rv = {}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(
                out,
                "    jbooleanArray result = (*env)->NewBooleanArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (result && rv) {{ (*env)->SetBooleanArrayRegion(env, result, 0, (jsize)out_len, (const jboolean*)rv); }}");
            let _ = writeln!(out, "    return result;");
        }
        _ => {
            let _ = writeln!(
                out,
                "    const void* rv = {}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(out, returns);
            release_jni_resources(out, params);
            let _ = writeln!(out, "    return NULL;");
        }
    }
}

fn write_map_return(
    out: &mut String,
    key: &TypeRef,
    val: &TypeRef,
    c_sym: &str,
    args_str: &str,
    returns: Option<&TypeRef>,
    params: &[weaveffi_ir::ir::Param],
) {
    let key_c = map_elem_c_type(key);
    let val_c = map_elem_c_type(val);
    let _ = writeln!(out, "    size_t out_map_len = 0;");
    let _ = writeln!(out, "    {kc}* out_keys = NULL;", kc = key_c);
    let _ = writeln!(out, "    {vc}* out_vals = NULL;", vc = val_c);
    if args_str.is_empty() {
        let _ = writeln!(
            out,
            "    {}(out_keys, out_vals, &out_map_len, &err);",
            c_sym
        );
    } else {
        let _ = writeln!(
            out,
            "    {}({}, out_keys, out_vals, &out_map_len, &err);",
            c_sym, args_str
        );
    }
    write_error_check(out, returns);
    release_jni_resources(out, params);
    let _ = writeln!(
        out,
        "    jclass hm_cls = (*env)->FindClass(env, \"java/util/HashMap\");"
    );
    let _ = writeln!(out, "    jobject result = (*env)->NewObject(env, hm_cls, (*env)->GetMethodID(env, hm_cls, \"<init>\", \"(I)V\"), (jint)out_map_len);");
    let _ = writeln!(out, "    jmethodID hm_put = (*env)->GetMethodID(env, hm_cls, \"put\", \"(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;\");");
    let _ = writeln!(out, "    for (size_t i = 0; i < out_map_len; i++) {{");
    write_map_box_elem(out, key, "jkey", "out_keys");
    write_map_box_elem(out, val, "jval", "out_vals");
    let _ = writeln!(
        out,
        "        (*env)->CallObjectMethod(env, result, hm_put, jkey, jval);"
    );
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "    return result;");
}

fn write_map_box_elem(out: &mut String, ty: &TypeRef, var: &str, arr: &str) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "        jstring {v} = (*env)->NewStringUTF(env, {a}[i]);",
                v = var,
                a = arr
            );
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "        jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                v = var
            );
            let _ = writeln!(
                out,
                "        jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(I)Ljava/lang/Integer;\"), (jint){a}[i]);",
                v = var,
                a = arr
            );
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            let _ = writeln!(
                out,
                "        jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                v = var
            );
            let _ = writeln!(
                out,
                "        jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\"), (jlong){a}[i]);",
                v = var,
                a = arr
            );
        }
        TypeRef::F64 => {
            let _ = writeln!(
                out,
                "        jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                v = var
            );
            let _ = writeln!(
                out,
                "        jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(D)Ljava/lang/Double;\"), (jdouble){a}[i]);",
                v = var,
                a = arr
            );
        }
        TypeRef::Bool => {
            let _ = writeln!(
                out,
                "        jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                v = var
            );
            let _ = writeln!(
                out,
                "        jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\"), {a}[i]);",
                v = var,
                a = arr
            );
        }
        _ => {
            let _ = writeln!(
                out,
                "        jobject {v} = (jobject){a}[i];",
                v = var,
                a = arr
            );
        }
    }
}

fn write_error_check(out: &mut String, ret_type: Option<&TypeRef>) {
    let _ = writeln!(out, "    if (err.code != 0) {{");
    let _ = writeln!(out, "        throw_weaveffi_error(env, &err);");
    let _ = writeln!(out, "        {}", jni_default_return(ret_type));
    let _ = writeln!(out, "    }}");
}

fn release_jni_resources(out: &mut String, params: &[weaveffi_ir::ir::Param]) {
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                    n = p.name
                );
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                    n = p.name
                );
            }
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    let _ = writeln!(
                        out,
                        "    if ({n} != NULL) {{ (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars); }}",
                        n = p.name
                    );
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => {
                    let _ = writeln!(
                        out,
                        "    if ({n} != NULL && {n}_elems != NULL) {{ (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0); }}",
                        n = p.name
                    );
                }
                _ => {}
            },
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::I32 | TypeRef::Enum(_) => {
                    let _ = writeln!(
                        out,
                        "    (*env)->ReleaseIntArrayElements(env, {n}, {n}_elems, 0);",
                        n = p.name
                    );
                }
                TypeRef::U32
                | TypeRef::I64
                | TypeRef::TypedHandle(_)
                | TypeRef::Handle
                | TypeRef::Struct(_) => {
                    let _ = writeln!(
                        out,
                        "    (*env)->ReleaseLongArrayElements(env, {n}, {n}_elems, 0);",
                        n = p.name
                    );
                }
                TypeRef::F64 => {
                    let _ = writeln!(
                        out,
                        "    (*env)->ReleaseDoubleArrayElements(env, {n}, {n}_elems, 0);",
                        n = p.name
                    );
                }
                TypeRef::Bool => {
                    let _ = writeln!(
                        out,
                        "    (*env)->ReleaseBooleanArrayElements(env, {n}, {n}_elems, 0);",
                        n = p.name
                    );
                }
                _ => {}
            },
            TypeRef::Map(k, v) => write_map_release(out, &p.name, k, v),
            _ => {}
        }
    }
}

fn write_map_release(out: &mut String, name: &str, key: &TypeRef, val: &TypeRef) {
    if matches!(key, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let _ = writeln!(
            out,
            "    for (jsize {n}_ri = 0; {n}_ri < {n}_len; {n}_ri++) {{",
            n = name
        );
        let _ = writeln!(
            out,
            "        (*env)->ReleaseStringUTFChars(env, {n}_jk[{n}_ri], {n}_c_keys[{n}_ri]);",
            n = name
        );
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "    free({n}_jk);", n = name);
    }
    if matches!(val, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let _ = writeln!(
            out,
            "    for (jsize {n}_ri = 0; {n}_ri < {n}_len; {n}_ri++) {{",
            n = name
        );
        let _ = writeln!(
            out,
            "        (*env)->ReleaseStringUTFChars(env, {n}_jv[{n}_ri], {n}_c_vals[{n}_ri]);",
            n = name
        );
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "    free({n}_jv);", n = name);
    }
    let _ = writeln!(out, "    free((void*){n}_c_keys);", n = name);
    let _ = writeln!(out, "    free((void*){n}_c_vals);", n = name);
}

fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

fn kotlin_getter_type(t: &TypeRef) -> String {
    match t {
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Enum(name) => name.clone(),
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback Android type")
        }
        other => kotlin_type(other),
    }
}

fn render_kotlin_struct(out: &mut String, s: &StructDef, c_prefix: &str) {
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "class {} internal constructor(private var handle: Long) : java.io.Closeable {{",
        s.name
    );
    let _ = writeln!(out, "    companion object {{");
    let _ = writeln!(out, "        init {{ System.loadLibrary(\"{c_prefix}\") }}");
    let _ = writeln!(out);

    let create_params: Vec<String> = s
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name, kotlin_type(&f.ty)))
        .collect();
    let _ = writeln!(
        out,
        "        @JvmStatic external fun nativeCreate({}): Long",
        create_params.join(", ")
    );
    let _ = writeln!(
        out,
        "        @JvmStatic external fun nativeDestroy(handle: Long)"
    );
    for f in &s.fields {
        let pascal = to_pascal_case(&f.name);
        let _ = writeln!(
            out,
            "        @JvmStatic external fun nativeGet{}(handle: Long): {}",
            pascal,
            kotlin_type(&f.ty)
        );
    }

    let param_names: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "        fun create({}): {} = {}(nativeCreate({}))",
        create_params.join(", "),
        s.name,
        s.name,
        param_names.join(", ")
    );
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);

    for f in &s.fields {
        let pascal = to_pascal_case(&f.name);
        let kt_type = kotlin_getter_type(&f.ty);
        match &f.ty {
            TypeRef::Struct(name) => {
                let local = local_type_name(name);
                let _ = writeln!(
                    out,
                    "    val {}: {} get() = {}(nativeGet{}(handle))",
                    f.name, kt_type, local, pascal
                );
            }
            _ => {
                let _ = writeln!(
                    out,
                    "    val {}: {} get() = nativeGet{}(handle)",
                    f.name, kt_type, pascal
                );
            }
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "    override fun close() {{");
    let _ = writeln!(out, "        if (handle != 0L) {{");
    let _ = writeln!(out, "            nativeDestroy(handle)");
    let _ = writeln!(out, "            handle = 0L");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out);
    let _ = writeln!(out, "    protected fun finalize() {{");
    let _ = writeln!(out, "        close()");
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
}

fn render_kotlin_builder(out: &mut String, s: &StructDef) {
    if !s.builder {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "class {}Builder {{", s.name);
    for f in &s.fields {
        let kt_getter = kotlin_getter_type(&f.ty);
        let _ = writeln!(out, "    private var {}: {}? = null", f.name, kt_getter);
    }
    for f in &s.fields {
        let pascal = to_pascal_case(&f.name);
        let kt_getter = kotlin_getter_type(&f.ty);
        let _ = writeln!(
            out,
            "    fun with{}({}: {}): {}Builder {{",
            pascal, f.name, kt_getter, s.name
        );
        let _ = writeln!(out, "        this.{} = {}", f.name, f.name);
        let _ = writeln!(out, "        return this");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "    fun build(): {} {{", s.name);
    if s.fields.is_empty() {
        let _ = writeln!(out, "        return {}.create()", s.name);
    } else {
        let _ = writeln!(out, "        return {}.create(", s.name);
        let n = s.fields.len();
        for (i, f) in s.fields.iter().enumerate() {
            let arg = format!(
                "{} ?: throw IllegalStateException(\"missing field: {}\")",
                f.name, f.name
            );
            let suffix = if i + 1 < n { "," } else { "" };
            let _ = writeln!(out, "            {}{}", arg, suffix);
        }
        let _ = writeln!(out, "        )");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
}

fn render_jni_struct(out: &mut String, module_name: &str, s: &StructDef, jni_prefix: &str) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    // nativeCreate
    {
        let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
        for f in &s.fields {
            jparams.push(format!("{} {}", jni_param_type(&f.ty), f.name));
        }
        let _ = writeln!(
            out,
            "JNIEXPORT jlong JNICALL Java_{}_{}_nativeCreate({}) {{",
            jni_prefix,
            s.name,
            jparams.join(", ")
        );
        let _ = writeln!(out, "    weaveffi_error err = {{0, NULL}};");

        for f in &s.fields {
            write_param_acquire(out, &f.name, &f.ty);
        }

        let mut call_args: Vec<String> = Vec::new();
        for f in &s.fields {
            build_c_call_args(&mut call_args, &f.name, &f.ty, module_name);
        }

        if call_args.is_empty() {
            let _ = writeln!(out, "    {}* rv = {}_create(&err);", prefix, prefix);
        } else {
            let _ = writeln!(
                out,
                "    {}* rv = {}_create({}, &err);",
                prefix,
                prefix,
                call_args.join(", ")
            );
        }
        write_error_check(out, Some(&TypeRef::Handle));

        for f in &s.fields {
            release_jni_resources_single(out, &f.name, &f.ty);
        }

        let _ = writeln!(out, "    return (jlong)(intptr_t)rv;");
        let _ = writeln!(out, "}}\n");
    }

    // nativeDestroy
    {
        let _ = writeln!(
            out,
            "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
            jni_prefix,
            s.name
        );
        let _ = writeln!(
            out,
            "    {}_destroy(({}*)(intptr_t)handle);",
            prefix, prefix
        );
        let _ = writeln!(out, "}}\n");
    }

    // nativeGet{Field} for each field
    for f in &s.fields {
        let pascal = to_pascal_case(&f.name);
        let jret = jni_ret_type(Some(&f.ty));
        let getter_c = format!("{}_get_{}", prefix, f.name);

        let _ = writeln!(
            out,
            "JNIEXPORT {} JNICALL Java_{}_{}_nativeGet{}(JNIEnv* env, jclass clazz, jlong handle) {{",
            jret, jni_prefix, s.name, pascal
        );

        match &f.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let _ = writeln!(
                    out,
                    "    const char* rv = {}((const {}*)(intptr_t)handle);",
                    getter_c, prefix
                );
                let _ = writeln!(
                    out,
                    "    jstring jout = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");"
                );
                let _ = writeln!(out, "    weaveffi_free_string(rv);");
                let _ = writeln!(out, "    return jout;");
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let _ = writeln!(out, "    size_t out_len = 0;");
                let _ = writeln!(
                    out,
                    "    uint8_t* rv = {}((const {}*)(intptr_t)handle, &out_len);",
                    getter_c, prefix
                );
                let _ = writeln!(
                    out,
                    "    jbyteArray jout = (*env)->NewByteArray(env, (jsize)out_len);"
                );
                let _ = writeln!(
                    out,
                    "    if (jout && rv) {{ (*env)->SetByteArrayRegion(env, jout, 0, (jsize)out_len, (const jbyte*)rv); }}"
                );
                let _ = writeln!(out, "    weaveffi_free_bytes(rv, (size_t)out_len);");
                let _ = writeln!(out, "    return jout;");
            }
            TypeRef::Bool => {
                let _ = writeln!(
                    out,
                    "    bool rv = {}((const {}*)(intptr_t)handle);",
                    getter_c, prefix
                );
                let _ = writeln!(out, "    return rv ? JNI_TRUE : JNI_FALSE;");
            }
            TypeRef::Struct(name) => {
                let c_struct =
                    weaveffi_core::utils::c_abi_struct_name(name, module_name, "weaveffi");
                let _ = writeln!(
                    out,
                    "    const {c_struct}* rv = {getter_c}((const {prefix}*)(intptr_t)handle);",
                    c_struct = c_struct,
                    getter_c = getter_c,
                    prefix = prefix
                );
                let _ = writeln!(out, "    return (jlong)(intptr_t)rv;");
            }
            TypeRef::Optional(inner) => {
                write_struct_optional_getter(out, inner, &getter_c, &prefix);
            }
            TypeRef::List(inner) => {
                write_struct_list_getter(out, inner, &getter_c, &prefix);
            }
            TypeRef::Callback(_) => {
                unreachable!("validator should have rejected callback Android type")
            }
            other => {
                let c_ty = c_type_for_return(other);
                let jcast = jni_cast_for(other);
                let _ = writeln!(
                    out,
                    "    {} rv = {}((const {}*)(intptr_t)handle);",
                    c_ty, getter_c, prefix
                );
                let _ = writeln!(out, "    return {}rv;", jcast);
            }
        }

        let _ = writeln!(out, "}}\n");
    }
}

fn write_struct_optional_getter(out: &mut String, inner: &TypeRef, getter_c: &str, prefix: &str) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "    const char* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            );
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(out, "    jstring jout = (*env)->NewStringUTF(env, rv);");
            let _ = writeln!(out, "    weaveffi_free_string(rv);");
            let _ = writeln!(out, "    return jout;");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "    const int32_t* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            );
            let _ = writeln!(out, "    if (rv == NULL) {{ return NULL; }}");
            let _ = writeln!(
                out,
                "    jclass cls = (*env)->FindClass(env, \"java/lang/Integer\");"
            );
            let _ = writeln!(
                out,
                "    jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(I)Ljava/lang/Integer;\");"
            );
            let _ = writeln!(
                out,
                "    return (*env)->CallStaticObjectMethod(env, cls, mid, (jint)*rv);"
            );
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            let _ = writeln!(
                out,
                "    const void* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            );
            let _ = writeln!(out, "    if (!rv) {{ return 0; }}");
            let _ = writeln!(out, "    return (jlong)(intptr_t)rv;");
        }
        _ => {
            let _ = writeln!(
                out,
                "    const void* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            );
            let _ = writeln!(out, "    if (!rv) {{ return 0; }}");
            let _ = writeln!(out, "    return (jlong)(intptr_t)rv;");
        }
    }
}

fn write_struct_list_getter(out: &mut String, inner: &TypeRef, getter_c: &str, prefix: &str) {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => {
            let _ = writeln!(out, "    size_t out_len = 0;");
            let _ = writeln!(
                out,
                "    const int32_t* rv = {}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            );
            let _ = writeln!(
                out,
                "    jintArray jout = (*env)->NewIntArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (jout && rv) {{ (*env)->SetIntArrayRegion(env, jout, 0, (jsize)out_len, (const jint*)rv); }}");
            let _ = writeln!(out, "    return jout;");
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            let _ = writeln!(out, "    size_t out_len = 0;");
            let _ = writeln!(
                out,
                "    const int64_t* rv = (const int64_t*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            );
            let _ = writeln!(
                out,
                "    jlongArray jout = (*env)->NewLongArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (jout && rv) {{ (*env)->SetLongArrayRegion(env, jout, 0, (jsize)out_len, (const jlong*)rv); }}");
            let _ = writeln!(out, "    return jout;");
        }
        TypeRef::F64 => {
            let _ = writeln!(out, "    size_t out_len = 0;");
            let _ = writeln!(
                out,
                "    const double* rv = (const double*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            );
            let _ = writeln!(
                out,
                "    jdoubleArray jout = (*env)->NewDoubleArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (jout && rv) {{ (*env)->SetDoubleArrayRegion(env, jout, 0, (jsize)out_len, (const jdouble*)rv); }}");
            let _ = writeln!(out, "    return jout;");
        }
        TypeRef::Bool => {
            let _ = writeln!(out, "    size_t out_len = 0;");
            let _ = writeln!(
                out,
                "    const int32_t* rv = (const int32_t*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            );
            let _ = writeln!(
                out,
                "    jbooleanArray jout = (*env)->NewBooleanArray(env, (jsize)out_len);"
            );
            let _ = writeln!(out, "    if (jout && rv) {{");
            let _ = writeln!(out, "        for (jsize i = 0; i < (jsize)out_len; i++) {{");
            let _ = writeln!(
                out,
                "            jboolean val = rv[i] ? JNI_TRUE : JNI_FALSE;"
            );
            let _ = writeln!(
                out,
                "            (*env)->SetBooleanArrayRegion(env, jout, i, 1, &val);"
            );
            let _ = writeln!(out, "        }}");
            let _ = writeln!(out, "    }}");
            let _ = writeln!(out, "    return jout;");
        }
        _ => {
            let _ = writeln!(out, "    size_t out_len = 0;");
            let _ = writeln!(
                out,
                "    const void* rv = {}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            );
            let _ = writeln!(out, "    (void)rv; (void)out_len;");
            let _ = writeln!(out, "    return NULL;");
        }
    }
}

fn release_jni_resources_single(out: &mut String, name: &str, ty: &TypeRef) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "    (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                n = name
            );
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(
                out,
                "    (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            );
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let _ = writeln!(
                    out,
                    "    if ({n} != NULL) {{ (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars); }}",
                    n = name
                );
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let _ = writeln!(
                    out,
                    "    if ({n} != NULL && {n}_elems != NULL) {{ (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0); }}",
                    n = name
                );
            }
            _ => {}
        },
        TypeRef::List(inner) => match inner.as_ref() {
            TypeRef::I32 | TypeRef::Enum(_) => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseIntArrayElements(env, {n}, {n}_elems, 0);",
                    n = name
                );
            }
            TypeRef::U32
            | TypeRef::I64
            | TypeRef::TypedHandle(_)
            | TypeRef::Handle
            | TypeRef::Struct(_) => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseLongArrayElements(env, {n}, {n}_elems, 0);",
                    n = name
                );
            }
            TypeRef::F64 => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseDoubleArrayElements(env, {n}, {n}_elems, 0);",
                    n = name
                );
            }
            TypeRef::Bool => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseBooleanArrayElements(env, {n}, {n}_elems, 0);",
                    n = name
                );
            }
            _ => {}
        },
        TypeRef::Map(k, v) => write_map_release(out, name, k, v),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules,
            generators: None,
        }
    }

    fn make_struct_api() -> Api {
        make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn kotlin_struct_class_declaration() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("class Contact internal constructor(private var handle: Long) : java.io.Closeable {"),
            "missing struct class declaration: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_create() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("@JvmStatic external fun nativeCreate(name: String, age: Int): Long"),
            "missing nativeCreate: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_destroy() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("@JvmStatic external fun nativeDestroy(handle: Long)"),
            "missing nativeDestroy: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_getters() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("@JvmStatic external fun nativeGetName(handle: Long): String"),
            "missing nativeGetName: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic external fun nativeGetAge(handle: Long): Int"),
            "missing nativeGetAge: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_factory_method() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains(
                "fun create(name: String, age: Int): Contact = Contact(nativeCreate(name, age))"
            ),
            "missing create factory: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_property_getters() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("val name: String get() = nativeGetName(handle)"),
            "missing name property: {kt}"
        );
        assert!(
            kt.contains("val age: Int get() = nativeGetAge(handle)"),
            "missing age property: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_closeable() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("override fun close()"),
            "missing close method: {kt}"
        );
        assert!(
            kt.contains("nativeDestroy(handle)"),
            "missing nativeDestroy call in close: {kt}"
        );
        assert!(kt.contains("handle = 0L"), "missing handle zeroing: {kt}");
    }

    #[test]
    fn kotlin_struct_finalize() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("protected fun finalize()"),
            "missing finalize: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_loads_library() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        let struct_section = kt.split("class Contact").nth(1).unwrap();
        assert!(
            struct_section.contains("System.loadLibrary(\"weaveffi\")"),
            "struct companion missing loadLibrary: {kt}"
        );
    }

    #[test]
    fn kotlin_builder_generated() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        AndroidGenerator.generate(&api, out).unwrap();
        let kotlin =
            std::fs::read_to_string(out.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();
        assert!(
            kotlin.contains("class ContactBuilder"),
            "missing builder class: {kotlin}"
        );
        assert!(
            kotlin.contains("fun withName("),
            "missing withName: {kotlin}"
        );
        assert!(kotlin.contains("fun withAge("), "missing withAge: {kotlin}");
        assert!(kotlin.contains("fun build()"), "missing build: {kotlin}");
    }

    #[test]
    fn jni_struct_native_create() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeCreate"),
            "missing JNI nativeCreate: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_create("),
            "missing C create call: {jni}"
        );
    }

    #[test]
    fn jni_struct_native_destroy() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeDestroy"),
            "missing JNI nativeDestroy: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_destroy("),
            "missing C destroy call: {jni}"
        );
    }

    #[test]
    fn jni_struct_native_getters() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetName"),
            "missing JNI nativeGetName: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_name("),
            "missing C get_name call: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetAge"),
            "missing JNI nativeGetAge: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_age("),
            "missing C get_age call: {jni}"
        );
    }

    #[test]
    fn jni_struct_string_getter_frees() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("weaveffi_free_string(rv)"),
            "missing free_string in getter: {jni}"
        );
    }

    #[test]
    fn jni_struct_create_handles_string_param() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("GetStringUTFChars(env, name, NULL)"),
            "missing string acquisition in create: {jni}"
        );
        assert!(
            jni.contains("ReleaseStringUTFChars(env, name, name_chars)"),
            "missing string release in create: {jni}"
        );
    }

    #[test]
    fn jni_struct_create_error_check() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        let create_section: &str = jni
            .split("Java_com_weaveffi_Contact_nativeCreate")
            .nth(1)
            .unwrap();
        assert!(
            create_section.contains("if (err.code != 0)"),
            "missing error check in create: {jni}"
        );
    }

    #[test]
    fn kotlin_struct_with_bytes_field() {
        let api = make_api(vec![Module {
            name: "storage".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Blob".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "data".to_string(),
                    ty: TypeRef::Bytes,
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
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("val data: ByteArray get() = nativeGetData(handle)"),
            "missing bytes property: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("weaveffi_storage_Blob_get_data("),
            "missing bytes getter C call: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes("),
            "missing free_bytes in getter: {jni}"
        );
    }

    #[test]
    fn kotlin_struct_with_nested_struct_field() {
        let api = make_api(vec![Module {
            name: "geo".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Line".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "start".to_string(),
                    ty: TypeRef::Struct("Point".into()),
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
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("val start: Point get() = Point(nativeGetStart(handle))"),
            "missing nested struct property: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("weaveffi_geo_Line_get_start("),
            "missing nested struct getter C call: {jni}"
        );
    }

    #[test]
    fn kotlin_type_for_struct_returns_long() {
        assert_eq!(kotlin_type(&TypeRef::Struct("Contact".into())), "Long");
    }

    #[test]
    fn kotlin_getter_type_for_struct_returns_name() {
        assert_eq!(
            kotlin_getter_type(&TypeRef::Struct("Contact".into())),
            "Contact"
        );
    }

    #[test]
    fn to_pascal_case_converts_snake_case() {
        assert_eq!(to_pascal_case("first_name"), "FirstName");
        assert_eq!(to_pascal_case("name"), "Name");
        assert_eq!(to_pascal_case("is_active"), "IsActive");
    }

    #[test]
    fn function_with_struct_param_jni() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "save".to_string(),
                params: vec![Param {
                    name: "contact".to_string(),
                    ty: TypeRef::Struct("Contact".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("contact: Long"),
            "missing struct param as Long: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("(const weaveffi_contacts_Contact*)(intptr_t)contact"),
            "missing struct param cast: {jni}"
        );
    }

    #[test]
    fn function_returning_struct_jni() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create".to_string(),
                params: vec![Param {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("weaveffi_contacts_Contact* rv"),
            "missing struct return type: {jni}"
        );
        assert!(
            jni.contains("return (jlong)(intptr_t)rv;"),
            "missing struct return cast: {jni}"
        );
    }

    // --- Enum tests ---

    #[test]
    fn kotlin_enum_class_generated() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("enum class Color(val value: Int) {"),
            "missing enum class: {kt}"
        );
        assert!(kt.contains("Red(0),"), "missing Red variant: {kt}");
        assert!(kt.contains("Green(1),"), "missing Green variant: {kt}");
        assert!(
            kt.contains("Blue(2);"),
            "missing Blue variant (with semicolon): {kt}"
        );
        assert!(
            kt.contains("companion object {"),
            "missing companion object: {kt}"
        );
        assert!(
            kt.contains("fun fromValue(value: Int): Color"),
            "missing fromValue: {kt}"
        );
    }

    #[test]
    fn kotlin_type_for_enum_is_int() {
        assert_eq!(kotlin_type(&TypeRef::Enum("Color".into())), "Int");
    }

    #[test]
    fn kotlin_getter_type_for_enum_returns_name() {
        assert_eq!(kotlin_getter_type(&TypeRef::Enum("Color".into())), "Color");
    }

    #[test]
    fn function_with_enum_param_kotlin() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "set_color".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("color: Color"),
            "public wrapper should use enum class name: {kt}"
        );
        assert!(
            kt.contains("private external fun set_colorJni(color: Int)"),
            "native function should use Int for JNI: {kt}"
        );
        assert!(
            kt.contains("color.value"),
            "wrapper should call .value on enum param: {kt}"
        );
    }

    #[test]
    fn kotlin_function_uses_enum_type() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "add_contact".to_string(),
                params: vec![
                    Param {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                    Param {
                        name: "contact_type".to_string(),
                        ty: TypeRef::Enum("ContactType".into()),
                        mutable: false,
                    },
                ],
                returns: Some(TypeRef::Enum("ContactType".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("contact_type: ContactType"),
            "public signature should use enum class name, not Int: {kt}"
        );
        assert!(
            kt.contains("): ContactType"),
            "return type should use enum class name: {kt}"
        );
        assert!(
            !kt.contains("external fun add_contact("),
            "public function should not be external: {kt}"
        );
        assert!(
            kt.contains("private external fun add_contactJni("),
            "native function should be private: {kt}"
        );
        assert!(
            kt.contains("contact_type.value"),
            "wrapper should extract int via .value: {kt}"
        );
        assert!(
            kt.contains("ContactType.fromValue("),
            "wrapper should wrap return in fromValue: {kt}"
        );
    }

    #[test]
    fn function_with_enum_param_jni() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "set_color".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("jint color"),
            "missing jint param in JNI: {jni}"
        );
        assert!(
            jni.contains("(int32_t)color"),
            "missing int32_t cast: {jni}"
        );
        assert!(
            jni.contains("WeaveFFI_set_colorJni("),
            "JNI function name should have Jni suffix for enum functions: {jni}"
        );
    }

    #[test]
    fn function_returning_enum_jni() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "get_color".to_string(),
                params: vec![],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jint JNICALL"),
            "missing jint return in JNI: {jni}"
        );
        assert!(jni.contains("(jint)"), "missing jint cast: {jni}");
        assert!(
            jni.contains("WeaveFFI_get_colorJni("),
            "JNI function name should have Jni suffix for enum functions: {jni}"
        );
    }

    // --- Optional tests ---

    #[test]
    fn kotlin_type_for_optional_int() {
        assert_eq!(
            kotlin_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "Int?"
        );
    }

    #[test]
    fn kotlin_type_for_optional_string() {
        assert_eq!(
            kotlin_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "String?"
        );
    }

    #[test]
    fn function_with_optional_int_param_kotlin() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(kt.contains("id: Int?"), "missing optional Int? param: {kt}");
    }

    #[test]
    fn function_with_optional_int_param_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(jni.contains("jobject id"), "missing jobject param: {jni}");
        assert!(
            jni.contains("java/lang/Integer"),
            "missing Integer class lookup: {jni}"
        );
        assert!(
            jni.contains("intValue"),
            "missing intValue unbox call: {jni}"
        );
        assert!(jni.contains("id_ptr"), "missing id_ptr in C call: {jni}");
    }

    #[test]
    fn function_with_optional_string_param_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find_name".to_string(),
                params: vec![Param {
                    name: "query".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("jstring query"),
            "missing jstring param: {jni}"
        );
        assert!(
            jni.contains("if (query != NULL)"),
            "missing null check for optional string: {jni}"
        );
    }

    #[test]
    fn function_returning_optional_int_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "lookup".to_string(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jobject JNICALL"),
            "missing jobject return: {jni}"
        );
        assert!(
            jni.contains("if (rv == NULL) { return NULL; }"),
            "missing NULL check: {jni}"
        );
        assert!(
            jni.contains("java/lang/Integer"),
            "missing Integer boxing: {jni}"
        );
        assert!(
            jni.contains("valueOf"),
            "missing valueOf boxing call: {jni}"
        );
    }

    #[test]
    fn function_returning_optional_string_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_name".to_string(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jstring JNICALL"),
            "missing jstring return: {jni}"
        );
        assert!(
            jni.contains("if (rv == NULL) { return NULL; }"),
            "missing NULL check: {jni}"
        );
        assert!(jni.contains("NewStringUTF"), "missing NewStringUTF: {jni}");
    }

    // --- List tests ---

    #[test]
    fn kotlin_type_for_list_int() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "IntArray"
        );
    }

    #[test]
    fn kotlin_type_for_list_string() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "Array<String>"
        );
    }

    #[test]
    fn kotlin_type_for_list_enum() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
            "IntArray"
        );
    }

    #[test]
    fn function_with_list_int_param_kotlin() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "process".to_string(),
                params: vec![Param {
                    name: "ids".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(kt.contains("ids: IntArray"), "missing IntArray param: {kt}");
    }

    #[test]
    fn function_with_list_int_param_jni() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "process".to_string(),
                params: vec![Param {
                    name: "ids".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("jintArray ids"),
            "missing jintArray param: {jni}"
        );
        assert!(
            jni.contains("GetIntArrayElements(env, ids, NULL)"),
            "missing GetIntArrayElements: {jni}"
        );
        assert!(
            jni.contains("ReleaseIntArrayElements(env, ids, ids_elems, 0)"),
            "missing ReleaseIntArrayElements: {jni}"
        );
    }

    #[test]
    fn function_returning_list_int_jni() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "get_ids".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jintArray JNICALL"),
            "missing jintArray return: {jni}"
        );
        assert!(jni.contains("NewIntArray"), "missing NewIntArray: {jni}");
        assert!(
            jni.contains("SetIntArrayRegion"),
            "missing SetIntArrayRegion: {jni}"
        );
        assert!(jni.contains("out_len"), "missing out_len: {jni}");
    }

    #[test]
    fn jni_param_type_enum_is_jint() {
        assert_eq!(jni_param_type(&TypeRef::Enum("Color".into())), "jint");
    }

    #[test]
    fn jni_param_type_optional_int_is_jobject() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "jobject"
        );
    }

    #[test]
    fn jni_param_type_optional_string_is_jstring() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "jstring"
        );
    }

    #[test]
    fn jni_param_type_list_int_is_jintarray() {
        assert_eq!(
            jni_param_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "jintArray"
        );
    }

    #[test]
    fn jni_param_type_list_long_is_jlongarray() {
        assert_eq!(
            jni_param_type(&TypeRef::List(Box::new(TypeRef::I64))),
            "jlongArray"
        );
    }

    #[test]
    fn generate_android_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get_contact".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_android_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator.generate(&api, out_dir).unwrap();

        let kotlin =
            std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();

        assert!(
            kotlin.contains("enum class Color(val value: Int) {"),
            "missing enum class: {kotlin}"
        );
        assert!(kotlin.contains("Red(0),"), "missing Red variant: {kotlin}");
        assert!(
            kotlin.contains("Green(1),"),
            "missing Green variant: {kotlin}"
        );
        assert!(
            kotlin.contains("Blue(2);"),
            "missing Blue variant with semicolon: {kotlin}"
        );
        assert!(
            kotlin.contains("fun fromValue(value: Int): Color"),
            "missing fromValue: {kotlin}"
        );

        assert!(
            kotlin.contains(
                "class Contact internal constructor(private var handle: Long) : java.io.Closeable {"
            ),
            "missing struct class: {kotlin}"
        );
        assert!(
            kotlin.contains(
                "@JvmStatic external fun nativeCreate(name: String, email: String, age: Int): Long"
            ),
            "missing nativeCreate: {kotlin}"
        );
        assert!(
            kotlin.contains("val name: String get() = nativeGetName(handle)"),
            "missing name getter: {kotlin}"
        );
        assert!(
            kotlin.contains("val email: String get() = nativeGetEmail(handle)"),
            "missing email getter: {kotlin}"
        );
        assert!(
            kotlin.contains("val age: Int get() = nativeGetAge(handle)"),
            "missing age getter: {kotlin}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetName"),
            "missing JNI nativeGetName: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_name("),
            "missing C get_name call: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetEmail"),
            "missing JNI nativeGetEmail: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_email("),
            "missing C get_email call: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetAge"),
            "missing JNI nativeGetAge: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_age("),
            "missing C get_age call: {jni}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kotlin_type_for_map() {
        assert_eq!(
            kotlin_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Map<String, Int>"
        );
        assert_eq!(
            kotlin_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::F64)
            )),
            "Map<String, Double>"
        );
        assert_eq!(
            kotlin_type(&TypeRef::Map(
                Box::new(TypeRef::I32),
                Box::new(TypeRef::StringUtf8)
            )),
            "Map<Int, String>"
        );
    }

    #[test]
    fn function_with_map_param_kotlin() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "update_scores".to_string(),
                params: vec![Param {
                    name: "scores".to_string(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("scores: Map<String, Int>"),
            "missing Map<String, Int> param: {kt}"
        );
    }

    #[test]
    fn function_with_map_param_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "update_scores".to_string(),
                params: vec![Param {
                    name: "scores".to_string(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("jobject scores"),
            "missing jobject param: {jni}"
        );
        assert!(
            jni.contains("java/util/Map"),
            "missing Map class lookup: {jni}"
        );
        assert!(
            jni.contains("scores_c_keys"),
            "missing parallel keys array: {jni}"
        );
        assert!(
            jni.contains("scores_c_vals"),
            "missing parallel vals array: {jni}"
        );
        assert!(
            jni.contains("(size_t)scores_len"),
            "missing length arg: {jni}"
        );
    }

    #[test]
    fn android_build_gradle_has_cmake_config() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_android_build_gradle_cmake");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator.generate(&api, out_dir).unwrap();

        let gradle = std::fs::read_to_string(tmp.join("android/build.gradle")).unwrap();
        assert!(
            gradle.contains("externalNativeBuild"),
            "missing externalNativeBuild in build.gradle: {gradle}"
        );
        assert!(
            gradle.contains("path \"src/main/cpp/CMakeLists.txt\""),
            "missing cmake path in build.gradle: {gradle}"
        );
        assert!(
            gradle.contains("cppFlags \"\""),
            "missing cppFlags in build.gradle: {gradle}"
        );
        assert!(
            gradle.contains("namespace 'com.weaveffi'"),
            "missing namespace in build.gradle: {gradle}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn function_returning_map_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_scores".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jobject JNICALL"),
            "missing jobject return: {jni}"
        );
        assert!(
            jni.contains("java/util/HashMap"),
            "missing HashMap construction: {jni}"
        );
        assert!(jni.contains("out_map_len"), "missing out_map_len: {jni}");
    }

    #[test]
    fn android_custom_package() {
        let api = make_api(vec![Module {
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
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let config = GeneratorConfig {
            android_package: Some("com.mycompany.ffi".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_android_custom_package");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let kotlin_path = tmp.join("android/src/main/kotlin/com/mycompany/ffi/WeaveFFI.kt");
        assert!(
            kotlin_path.exists(),
            "Kotlin file not at custom package path"
        );

        let kotlin = std::fs::read_to_string(&kotlin_path).unwrap();
        assert!(
            kotlin.contains("package com.mycompany.ffi"),
            "missing custom package declaration: {kotlin}"
        );
        assert!(
            !kotlin.contains("package com.weaveffi"),
            "should not contain default package: {kotlin}"
        );

        let gradle = std::fs::read_to_string(tmp.join("android/build.gradle")).unwrap();
        assert!(
            gradle.contains("namespace 'com.mycompany.ffi'"),
            "missing custom namespace in build.gradle: {gradle}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
        assert!(
            jni.contains("Java_com_mycompany_ffi_WeaveFFI_math_add"),
            "missing custom JNI prefix: {jni}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kotlin_inline_error_types() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "ContactError".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "ContactNotFound".to_string(),
                        code: 1001,
                        message: "Contact not found".to_string(),
                    },
                    ErrorCode {
                        name: "InvalidInput".to_string(),
                        code: 1002,
                        message: "Invalid input provided".to_string(),
                    },
                ],
            }),
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("sealed class WeaveFFIException(val code: Int, message: String) : Exception(message)"),
            "missing sealed class declaration: {kt}"
        );
        assert!(
            kt.contains("class ContactNotFound(message: String = \"Contact not found\") : WeaveFFIException(1001, message)"),
            "missing ContactNotFound subclass: {kt}"
        );
        assert!(
            kt.contains("class InvalidInput(message: String = \"Invalid input provided\") : WeaveFFIException(1002, message)"),
            "missing InvalidInput subclass: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("throw_weaveffi_error(env, &err)"),
            "missing throw_weaveffi_error call: {jni}"
        );
        assert!(
            jni.contains("case 1001:"),
            "missing case for ContactNotFound: {jni}"
        );
        assert!(
            jni.contains("WeaveFFIException$ContactNotFound"),
            "missing FindClass for ContactNotFound: {jni}"
        );
        assert!(
            jni.contains("case 1002:"),
            "missing case for InvalidInput: {jni}"
        );
        assert!(
            jni.contains("WeaveFFIException$InvalidInput"),
            "missing FindClass for InvalidInput: {jni}"
        );
    }

    #[test]
    fn android_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create_contact".to_string(),
                params: vec![Param {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let config = GeneratorConfig {
            strip_module_prefix: true,
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_android_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        AndroidGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let kotlin =
            std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();

        assert!(
            kotlin.contains("fun create_contact("),
            "stripped name should be create_contact: {kotlin}"
        );
        assert!(
            !kotlin.contains("fun contacts_create_contact("),
            "should not contain module-prefixed name: {kotlin}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

        assert!(
            jni.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {jni}"
        );

        let no_strip = GeneratorConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_android_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        AndroidGenerator
            .generate_with_config(&api, out_dir2, &no_strip)
            .unwrap();

        let kotlin2 =
            std::fs::read_to_string(tmp2.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();

        assert!(
            kotlin2.contains("fun contacts_create_contact("),
            "default should use module-prefixed name: {kotlin2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn android_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
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
        }]);
        let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kotlin.contains("LongArray?"),
            "should contain deeply nested optional type: {kotlin}"
        );
    }

    #[test]
    fn android_map_of_lists() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "scores".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                    ),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kotlin.contains("Map<String, IntArray>"),
            "should contain map of lists type: {kotlin}"
        );
    }

    #[test]
    fn android_enum_keyed_map() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "contacts".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Struct("Contact".into())),
                    ),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kotlin.contains("Map<Int, Long>"),
            "should contain enum-keyed map type: {kotlin}"
        );
    }

    #[test]
    fn android_typed_handle_type() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_info".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
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
        }]);
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("contact: Contact"),
            "TypedHandle should use class type not Long: {kt}"
        );
    }

    #[test]
    fn android_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
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
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("GetStringUTFChars"),
            "input StringUtf8 should use GetStringUTFChars: {jni}"
        );
        assert!(
            jni.contains("ReleaseStringUTFChars"),
            "input StringUtf8 should release JVM chars: {jni}"
        );
        assert!(
            !jni.contains("weaveffi_free_string(name"),
            "input string param must not be freed via WeaveFFI: {jni}"
        );

        let start = jni
            .find("Java_com_weaveffi_WeaveFFI_find_contact")
            .expect("find_contact JNI symbol");
        let rest = &jni[start..];
        let end = rest.find("\nJNIEXPORT ").unwrap_or(rest.len());
        let fn_body = &rest[..end];
        let err_pos = fn_body
            .find("if (err.code != 0)")
            .expect("error check before using return value");
        let ret_pos = fn_body
            .find("(jlong)(intptr_t)rv")
            .expect("struct return as jlong handle");
        assert!(
            err_pos < ret_pos,
            "err check must precede struct return: {jni}"
        );
        assert!(
            fn_body.contains("throw_weaveffi_error"),
            "error path should throw: {jni}"
        );

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(kt.contains("class Contact"), "struct class Contact: {kt}");
    }

    #[test]
    fn android_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
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
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("if (rv == NULL)"),
            "optional struct return needs null check: {jni}"
        );
        assert!(
            jni.contains("return NULL"),
            "optional null should return NULL: {jni}"
        );
    }

    #[test]
    fn kotlin_async_function_is_suspend() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("suspend fun"),
            "async function should generate suspend fun: {kt}"
        );
        assert!(
            kt.contains("suspend fun run(id: Int): Int"),
            "suspend fun should have correct signature: {kt}"
        );
    }

    #[test]
    fn kotlin_async_uses_coroutine() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("suspendCancellableCoroutine"),
            "async function should use suspendCancellableCoroutine: {kt}"
        );
        assert!(
            kt.contains("WeaveContinuation"),
            "async function should use WeaveContinuation: {kt}"
        );
        assert!(
            kt.contains("import kotlinx.coroutines.suspendCancellableCoroutine"),
            "should import suspendCancellableCoroutine: {kt}"
        );
    }

    #[test]
    fn kotlin_cancellable_async_uses_invoke_on_cancellation() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![
                Function {
                    name: "run".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: true,
                    cancellable: true,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "fire".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("@JvmStatic private external fun weaveffiCancelTokenCreate(): Long"),
            "cancellable async must declare the cancel-token-create JNI helper: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic private external fun weaveffiCancelTokenCancel(token: Long)"),
            "cancellable async must declare the cancel-token-cancel JNI helper: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic private external fun weaveffiCancelTokenDestroy(token: Long)"),
            "cancellable async must declare the cancel-token-destroy JNI helper: {kt}"
        );
        assert!(
            kt.contains("val token = weaveffiCancelTokenCreate()"),
            "cancellable suspend fun must create a real token: {kt}"
        );
        assert!(
            kt.contains("suspendCancellableCoroutine"),
            "cancellable suspend fun must still use suspendCancellableCoroutine: {kt}"
        );
        assert!(
            kt.contains("cont.invokeOnCancellation { weaveffiCancelTokenCancel(token) }"),
            "cancellable suspend fun must wire invokeOnCancellation to the native token: {kt}"
        );
        assert!(
            kt.contains("runAsync(id, token, WeaveContinuation(cont))"),
            "cancellable Async JNI call must forward the real token: {kt}"
        );
        assert!(
            kt.contains("weaveffiCancelTokenDestroy(token)"),
            "cancellable suspend fun must destroy the token after completion: {kt}"
        );
        assert!(
            !kt.contains("runAsync(id, 0L, WeaveContinuation(cont))"),
            "cancellable Async JNI call must not pass a null-placeholder token: {kt}"
        );

        let fire_line = kt
            .lines()
            .find(|l| l.contains("fireAsync("))
            .expect("non-cancellable fireAsync call should be emitted");
        assert!(
            !fire_line.contains("token"),
            "non-cancellable async must not forward a cancel token: {fire_line}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_WeaveFFI_weaveffiCancelTokenCreate"),
            "JNI bridge must export cancel-token-create: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_WeaveFFI_weaveffiCancelTokenCancel"),
            "JNI bridge must export cancel-token-cancel: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_WeaveFFI_weaveffiCancelTokenDestroy"),
            "JNI bridge must export cancel-token-destroy: {jni}"
        );
        assert!(
            jni.contains("weaveffi_cancel_token_cancel((weaveffi_cancel_token*)(intptr_t)token)"),
            "JNI cancel-token-cancel must forward to the C ABI: {jni}"
        );
    }

    #[test]
    fn android_jni_string_param_uses_ptr_and_len() {
        // The C ABI expands a `string` parameter named `msg` to the pair
        // `(const uint8_t* msg_ptr, size_t msg_len)`, plus the trailing
        // `weaveffi_error* err`. The JNI bridge must acquire the UTF-8 chars
        // and length via JNI, then forward them to the C function with the
        // matching `(const uint8_t*)msg_chars, (size_t)msg_len, &err` casts.
        // The bridge must also include the generated `weaveffi.h` and the
        // CMake project must add the sibling `c/` directory to its include
        // path so the new C prototype resolves at compile time.
        let api = make_api(vec![Module {
            name: "echo".to_string(),
            functions: vec![Function {
                name: "shout".to_string(),
                params: vec![Param {
                    name: "msg".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_android_string_ptr_len");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator.generate(&api, out_dir).unwrap();

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
        assert!(
            jni.contains("#include \"weaveffi.h\""),
            "JNI bridge must include the generated C header so the new C prototype resolves: {jni}"
        );
        assert!(
            jni.contains("const char* msg_chars = (*env)->GetStringUTFChars(env, msg, NULL);"),
            "JNI bridge must acquire UTF-8 chars from the jstring: {jni}"
        );
        assert!(
            jni.contains("jsize msg_len = (*env)->GetStringUTFLength(env, msg);"),
            "JNI bridge must read the UTF-8 byte length from the jstring: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_echo_shout((const uint8_t*)msg_chars, (size_t)msg_len, &err);"
            ),
            "C call must pass ((const uint8_t*)msg_chars, (size_t)msg_len, &err) matching the new (const uint8_t*, size_t) C signature: {jni}"
        );
        assert!(
            jni.contains("(*env)->ReleaseStringUTFChars(env, msg, msg_chars);"),
            "JNI bridge must release the UTF-8 chars after the C call: {jni}"
        );

        // Arity guard: the call site must have exactly 3 top-level args
        // (ptr, len, err). Any regression that drops the `len` or reverts to
        // a single `const char*` parameter would trip this check.
        let call_start = jni
            .find("weaveffi_echo_shout(")
            .expect("weaveffi_echo_shout call site missing");
        let open = call_start + "weaveffi_echo_shout".len();
        let mut depth = 0i32;
        let mut end = open;
        for (i, &b) in jni.as_bytes().iter().enumerate().skip(open) {
            match b {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let args = &jni[open + 1..end];
        let mut top_commas = 0usize;
        let mut d = 0i32;
        for ch in args.chars() {
            match ch {
                '(' | '[' | '{' => d += 1,
                ')' | ']' | '}' => d -= 1,
                ',' if d == 0 => top_commas += 1,
                _ => {}
            }
        }
        assert_eq!(
            top_commas, 2,
            "call site arity must match the new C signature (ptr, len, err) = 3 args / 2 commas; got args: {args:?}"
        );

        // No legacy NUL-terminated/const char* convention may leak into the
        // generated JNI bridge: those would mismatch the new
        // (const uint8_t*, size_t) C prototype.
        assert!(
            !jni.contains("weaveffi_echo_shout(msg_chars, &err)")
                && !jni.contains("weaveffi_echo_shout(msg_chars,&err)"),
            "JNI bridge must NOT use the old single-pointer NUL-terminated form: {jni}"
        );
        assert!(
            !jni.contains("weaveffi_echo_shout((const char*)msg_chars,")
                && !jni.contains("weaveffi_echo_shout(msg,"),
            "JNI bridge must NOT pass msg_chars as a bare const char* or the raw jstring: {jni}"
        );

        let cmake =
            std::fs::read_to_string(tmp.join("android/src/main/cpp/CMakeLists.txt")).unwrap();
        assert!(
            cmake.contains("target_include_directories(weaveffi PRIVATE ../../../../c)"),
            "CMakeLists must add the generated c/ directory to the include path so weaveffi.h resolves: {cmake}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn android_bytes_param_uses_canonical_shape() {
        let api = make_api(vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "send".to_string(),
                params: vec![Param {
                    name: "payload".to_string(),
                    ty: TypeRef::Bytes,
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains(
                "weaveffi_io_send((const uint8_t*)payload_elems, (size_t)payload_len, &err);"
            ),
            "JNI must call C with canonical (const uint8_t*, size_t) shape for bytes param: {jni}"
        );
    }

    #[test]
    fn android_bytes_return_uses_canonical_shape() {
        let api = make_api(vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "read".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("size_t out_len = 0;"),
            "JNI must declare out_len for canonical size_t* out_len out-param: {jni}"
        );
        assert!(
            jni.contains("uint8_t* rv = weaveffi_io_read((int32_t)id, &out_len, &err);"),
            "JNI must capture C return as uint8_t* with (params..., &out_len, &err): {jni}"
        );
        assert!(
            !jni.contains("const uint8_t* rv = weaveffi_io_read("),
            "JNI must NOT declare bytes return as const (C ABI now returns non-const uint8_t*): {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes(rv, (size_t)out_len);"),
            "JNI must free bytes return directly without (uint8_t*) cast: {jni}"
        );
        assert!(
            !jni.contains("weaveffi_free_bytes((uint8_t*)rv"),
            "JNI must NOT need (uint8_t*) cast on bytes return: {jni}"
        );
    }

    #[test]
    fn android_throw_weaveffi_error_calls_error_clear() {
        let api = make_api(vec![Module {
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
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        let throw_fn_pos = jni
            .find("static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {")
            .expect("throw_weaveffi_error helper must be defined");
        let throw_new_pos = jni[throw_fn_pos..]
            .find("(*env)->ThrowNew(env, exClass, msg);")
            .map(|p| p + throw_fn_pos)
            .expect("throw_weaveffi_error must call ThrowNew with msg");
        let clear_pos = jni[throw_fn_pos..]
            .find("weaveffi_error_clear(err);")
            .map(|p| p + throw_fn_pos)
            .expect(
                "throw_weaveffi_error must call weaveffi_error_clear after capturing the message",
            );
        assert!(
            throw_new_pos < clear_pos,
            "weaveffi_error_clear must run AFTER ThrowNew has captured err->message: {jni}"
        );
    }

    #[test]
    fn android_bytes_return_calls_free_bytes() {
        // Cover both the synchronous JNI bridge and the async callback:
        // both must copy bytes into a jbyteArray (SetByteArrayRegion) before
        // releasing the owned C buffer via weaveffi_free_bytes.
        let api = make_api(vec![Module {
            name: "parity".to_string(),
            functions: vec![
                Function {
                    name: "echo".to_string(),
                    params: vec![Param {
                        name: "b".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "echo_async".to_string(),
                    params: vec![Param {
                        name: "b".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        let copy_pos = jni
            .find("SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv)")
            .expect("JNI must copy the returned bytes into a jbyteArray via SetByteArrayRegion");
        let free_pos = jni
            .find("weaveffi_free_bytes(rv, (size_t)out_len);")
            .expect("JNI must free the returned pointer via weaveffi_free_bytes");
        assert!(
            copy_pos < free_pos,
            "weaveffi_free_bytes must run AFTER copying data into the jbyteArray: {jni}"
        );

        let async_copy_pos = jni
            .find("SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result)")
            .expect("JNI async callback must copy the returned bytes into a jbyteArray");
        let async_free_pos = jni
            .find("weaveffi_free_bytes((uint8_t*)result, (size_t)result_len);")
            .expect("JNI async callback must free the returned pointer via weaveffi_free_bytes");
        assert!(
            async_copy_pos < async_free_pos,
            "weaveffi_free_bytes must run AFTER copying data into the jbyteArray in async callback: {jni}"
        );
    }

    #[test]
    fn android_struct_wrapper_calls_destroy() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("class Contact internal constructor(private var handle: Long) : java.io.Closeable {"),
            "Kotlin struct must implement Closeable: {kt}"
        );
        let close_pos = kt
            .find("override fun close()")
            .expect("Kotlin struct must override close");
        let destroy_pos = kt[close_pos..]
            .find("nativeDestroy(handle)")
            .map(|p| close_pos + p)
            .expect("close() must call nativeDestroy");
        let zero_pos = kt[close_pos..]
            .find("handle = 0L")
            .map(|p| close_pos + p)
            .expect("close() must zero the handle to be idempotent");
        assert!(destroy_pos > close_pos && zero_pos > destroy_pos);
        let finalize_pos = kt
            .find("protected fun finalize()")
            .expect("Kotlin struct must declare a finalize fallback");
        assert!(kt[finalize_pos..].contains("close()"));

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("weaveffi_contacts_Contact_destroy("),
            "JNI native destroy must call the C ABI destroy: {jni}"
        );
    }

    #[test]
    fn capabilities_includes_callbacks_and_listeners() {
        let caps = AndroidGenerator.capabilities();
        for cap in Capability::ALL {
            assert!(caps.contains(cap), "Android generator must support {cap:?}");
        }
    }

    #[test]
    fn kotlin_emits_callback_typealias_and_jni_registry() {
        use weaveffi_ir::ir::CallbackDef;
        let api = make_api(vec![Module {
            name: "events".to_string(),
            functions: vec![Function {
                name: "subscribe".to_string(),
                params: vec![Param {
                    name: "handler".to_string(),
                    ty: TypeRef::Callback("OnData".to_string()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnData".to_string(),
                params: vec![Param {
                    name: "payload".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("typealias OnData = (String) -> Unit"),
            "Kotlin must emit callback typealias: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic private external fun registerOnData(callback: OnData): Long"),
            "Kotlin must emit register external fun: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic private external fun subscribeJni(handler: Long)"),
            "JNI private external must take Long token for callback: {kt}"
        );
        assert!(
            kt.contains("subscribeJni(registerOnData(handler))"),
            "public wrapper must register lambda and pass token to Jni: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains("static JavaVM* g_weaveffi_jvm = NULL;"),
            "JNI must declare global JavaVM: {jni}"
        );
        assert!(
            jni.contains("typedef struct weaveffi_cb_entry {"),
            "JNI must declare registry entry struct: {jni}"
        );
        assert!(
            jni.contains("static jobject weaveffi_cb_lookup(jlong id)"),
            "JNI must declare registry lookup: {jni}"
        );
        assert!(
            jni.contains("static jlong weaveffi_cb_register(JNIEnv* env, jobject callback)"),
            "JNI must declare registry register helper: {jni}"
        );
        assert!(
            jni.contains("static void weaveffi_events_OnData_trampoline("),
            "JNI must declare per-callback trampoline: {jni}"
        );
        assert!(
            jni.contains("(*env)->CallVoidMethod(env, lambda, lambda_mid")
                || jni.contains("(*env)->CallObjectMethod(env, lambda, lambda_mid"),
            "trampoline must invoke lambda via CallVoidMethod or CallObjectMethod: {jni}"
        );
        assert!(
            jni.contains(
                "JNIEXPORT jlong JNICALL Java_com_weaveffi_WeaveFFI_registerOnData(JNIEnv* env, jclass clazz, jobject callback)"
            ),
            "JNI must export the register function: {jni}"
        );
        assert!(
            jni.contains("&weaveffi_events_OnData_trampoline"),
            "JNI wrapper must pass trampoline pointer to C ABI: {jni}"
        );
        assert!(
            jni.contains("(void*)(intptr_t)handler"),
            "JNI wrapper must pass registry token as context: {jni}"
        );
    }

    #[test]
    fn kotlin_emits_listener_class() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnData".to_string(),
                params: vec![Param {
                    name: "value".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "data_stream".to_string(),
                event_callback: "OnData".to_string(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            kt.contains("class DataStream {"),
            "missing listener class: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic external fun register(callback: OnData): Long"),
            "listener must expose register(callback): Long taking the event callback: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic external fun unregister(id: Long)"),
            "listener must expose unregister(id: Long): {kt}"
        );
        assert!(
            kt.contains("System.loadLibrary(\"weaveffi\")"),
            "listener companion must load the native library: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi");
        assert!(
            jni.contains(
                "JNIEXPORT jlong JNICALL Java_com_weaveffi_DataStream_register(JNIEnv* env, jclass clazz, jobject callback)"
            ),
            "JNI must export listener register: {jni}"
        );
        assert!(
            jni.contains("jlong token = weaveffi_cb_register(env, callback);"),
            "listener register must pin lambda via shared callback registry: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_events_register_data_stream(&weaveffi_events_OnData_trampoline, (void*)(intptr_t)token)"
            ),
            "listener register must call C register with trampoline and context token: {jni}"
        );
        assert!(
            jni.contains(
                "JNIEXPORT void JNICALL Java_com_weaveffi_DataStream_unregister(JNIEnv* env, jclass clazz, jlong id)"
            ),
            "JNI must export listener unregister: {jni}"
        );
        assert!(
            jni.contains("weaveffi_events_unregister_data_stream((uint64_t)id);"),
            "listener unregister must call C unregister with the listener id: {jni}"
        );
    }

    #[test]
    fn android_loads_native_lib_respects_c_prefix() {
        // When `c_prefix` is overridden, the Android generator must emit
        // Kotlin `System.loadLibrary("{c_prefix}")` for every companion object
        // (root WeaveFFI, struct wrappers, and listener wrappers), the JNI
        // bridge must `#include "{c_prefix}.h"`, and the CMakeLists.txt must
        // declare the shared library with the prefix-derived name so Gradle
        // builds the native artifact that `System.loadLibrary` resolves at
        // runtime.
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".to_string(),
            functions: vec![Function {
                name: "ping".to_string(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Subscription".to_string(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnTick".to_string(),
                params: vec![],
                returns: None,
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "tick".to_string(),
                event_callback: "OnTick".to_string(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let config = GeneratorConfig {
            c_prefix: Some("myffi".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_android_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let kt =
            std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();
        assert!(
            kt.contains("System.loadLibrary(\"myffi\")"),
            "Kotlin must loadLibrary using c_prefix: {kt}"
        );
        assert!(
            !kt.contains("System.loadLibrary(\"weaveffi\")"),
            "Kotlin must not retain the default weaveffi lib name when c_prefix is overridden: {kt}"
        );
        // The WeaveFFI root, struct, and listener companions each load the
        // native library independently, so every occurrence must be rewritten.
        assert_eq!(
            kt.matches("System.loadLibrary(\"myffi\")").count(),
            3,
            "every companion object (root + struct + listener) must loadLibrary via c_prefix: {kt}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
        assert!(
            jni.contains("#include \"myffi.h\""),
            "JNI bridge must include the c_prefix-named header: {jni}"
        );
        assert!(
            !jni.contains("#include \"weaveffi.h\""),
            "JNI bridge must not include the default weaveffi.h when c_prefix is overridden: {jni}"
        );

        let cmake =
            std::fs::read_to_string(tmp.join("android/src/main/cpp/CMakeLists.txt")).unwrap();
        assert!(
            cmake.contains("project(myffi)"),
            "CMakeLists must declare the project using c_prefix so the artifact matches System.loadLibrary: {cmake}"
        );
        assert!(
            cmake.contains("add_library(myffi SHARED weaveffi_jni.c)"),
            "CMakeLists must build lib{{c_prefix}}.so so System.loadLibrary(\"myffi\") resolves: {cmake}"
        );
        assert!(
            cmake.contains("target_include_directories(myffi PRIVATE ../../../../c)"),
            "CMakeLists include path directive must target the c_prefix-named library: {cmake}"
        );
        // Neither the CMake project name nor the add_library target may leak
        // the default `weaveffi` prefix; the source file `weaveffi_jni.c` is
        // an internal artifact, not a library reference.
        assert!(
            !cmake.contains("project(weaveffi)"),
            "CMake project() must not fall back to the default weaveffi name: {cmake}"
        );
        assert!(
            !cmake.contains("add_library(weaveffi"),
            "CMake add_library target must not fall back to the default weaveffi name: {cmake}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
