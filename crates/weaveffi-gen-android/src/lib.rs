use anyhow::Result;
use camino::Utf8Path;
use std::fmt::Write as _;
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, EnumDef, Function, StructDef, TypeRef};

pub struct AndroidGenerator;

impl AndroidGenerator {
    fn generate_impl(&self, api: &Api, out_dir: &Utf8Path, package: &str) -> Result<()> {
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
        let kotlin = render_kotlin(api, package);
        std::fs::write(src_dir.join("WeaveFFI.kt"), kotlin)?;

        let jni_dir = dir.join("src/main/cpp");
        std::fs::create_dir_all(&jni_dir)?;
        std::fs::write(jni_dir.join("CMakeLists.txt"), CMAKE)?;
        let jni_c = render_jni_c(api, package);
        std::fs::write(jni_dir.join("weaveffi_jni.c"), jni_c)?;

        Ok(())
    }
}

impl Generator for AndroidGenerator {
    fn name(&self) -> &'static str {
        "android"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "com.weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.android_package())
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

const CMAKE: &str = r#"cmake_minimum_required(VERSION 3.22)
project(weaveffi)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
"#;

fn kotlin_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I32 => "Int".to_string(),
        TypeRef::U32 => "Long".to_string(),
        TypeRef::I64 => "Long".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Boolean".to_string(),
        TypeRef::StringUtf8 => "String".to_string(),
        TypeRef::Bytes => "ByteArray".to_string(),
        TypeRef::Handle => "Long".to_string(),
        TypeRef::Struct(_) => "Long".to_string(),
        TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::Optional(inner) => format!("{}?", kotlin_type(inner)),
        TypeRef::List(inner) => kotlin_list_type(inner),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", kotlin_type(k), kotlin_type(v)),
    }
}

fn kotlin_list_type(inner: &TypeRef) -> String {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => "IntArray".to_string(),
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
            "LongArray".to_string()
        }
        TypeRef::F64 => "DoubleArray".to_string(),
        TypeRef::Bool => "BooleanArray".to_string(),
        TypeRef::StringUtf8 => "Array<String>".to_string(),
        TypeRef::Bytes => "Array<ByteArray>".to_string(),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => "LongArray".to_string(),
    }
}

fn jni_param_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I32 | TypeRef::Enum(_) => "jint".to_string(),
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => "jlong".to_string(),
        TypeRef::F64 => "jdouble".to_string(),
        TypeRef::Bool => "jboolean".to_string(),
        TypeRef::StringUtf8 => "jstring".to_string(),
        TypeRef::Bytes => "jbyteArray".to_string(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => "jstring".to_string(),
            TypeRef::Bytes => "jbyteArray".to_string(),
            _ => "jobject".to_string(),
        },
        TypeRef::List(inner) => jni_array_type(inner),
        TypeRef::Map(_, _) => "jobject".to_string(),
    }
}

fn jni_array_type(inner: &TypeRef) -> String {
    match inner {
        TypeRef::I32 | TypeRef::Enum(_) => "jintArray".to_string(),
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
            "jlongArray".to_string()
        }
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
        TypeRef::Handle => "weaveffi_handle_t",
        TypeRef::StringUtf8 => "const char*",
        TypeRef::Bytes => "const uint8_t*",
        TypeRef::Struct(_) | TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            "void*"
        }
    }
}

fn jni_default_return(t: Option<&TypeRef>) -> &'static str {
    match t {
        None => "",
        Some(TypeRef::I32 | TypeRef::Enum(_)) => "return 0;",
        Some(TypeRef::U32 | TypeRef::I64 | TypeRef::Handle) => "return 0;",
        Some(TypeRef::F64) => "return 0.0;",
        Some(TypeRef::Bool) => "return JNI_FALSE;",
        Some(TypeRef::StringUtf8) => "return NULL;",
        Some(TypeRef::Bytes) => "return NULL;",
        Some(TypeRef::Struct(_)) => "return 0;",
        Some(TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _)) => "return NULL;",
    }
}

fn jni_cast_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 | TypeRef::Enum(_) => "(jint)",
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => "(jlong)",
        TypeRef::F64 => "(jdouble)",
        TypeRef::Struct(_) => "(jlong)(intptr_t)",
        _ => "",
    }
}

fn kotlin_public_type(t: &TypeRef) -> String {
    match t {
        TypeRef::Enum(name) => name.clone(),
        other => kotlin_type(other),
    }
}

fn has_enum_involvement(f: &Function) -> bool {
    f.params.iter().any(|p| matches!(&p.ty, TypeRef::Enum(_)))
        || matches!(&f.returns, Some(TypeRef::Enum(_)))
}

fn render_kotlin(api: &Api, package: &str) -> String {
    let mut kotlin = format!("package {package}\n\nclass WeaveFFI {{\n    companion object {{\n        init {{ System.loadLibrary(\"weaveffi\") }}\n\n");
    for m in &api.modules {
        for f in &m.functions {
            if has_enum_involvement(f) {
                let native_params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, kotlin_type(&p.ty)))
                    .collect();
                let native_ret = f
                    .returns
                    .as_ref()
                    .map(kotlin_type)
                    .unwrap_or_else(|| "Unit".to_string());
                let _ = writeln!(
                    kotlin,
                    "        @JvmStatic private external fun {}Jni({}): {}",
                    f.name,
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
                    .map(|p| {
                        if matches!(&p.ty, TypeRef::Enum(_)) {
                            format!("{}.value", p.name)
                        } else {
                            p.name.clone()
                        }
                    })
                    .collect();
                let call = format!("{}Jni({})", f.name, call_args.join(", "));

                if let Some(TypeRef::Enum(name)) = &f.returns {
                    let _ = writeln!(
                        kotlin,
                        "        @JvmStatic fun {}({}): {} = {}.fromValue({})",
                        f.name,
                        public_params.join(", "),
                        public_ret,
                        name,
                        call
                    );
                } else if f.returns.is_some() {
                    let _ = writeln!(
                        kotlin,
                        "        @JvmStatic fun {}({}): {} = {}",
                        f.name,
                        public_params.join(", "),
                        public_ret,
                        call
                    );
                } else {
                    let _ = writeln!(
                        kotlin,
                        "        @JvmStatic fun {}({}) {{ {} }}",
                        f.name,
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
                    f.name,
                    params_sig.join(", "),
                    ret
                );
            }
        }
    }
    kotlin.push_str("    }\n}\n");
    for m in &api.modules {
        for e in &m.enums {
            render_kotlin_enum(&mut kotlin, e);
        }
        for s in &m.structs {
            render_kotlin_struct(&mut kotlin, s);
        }
    }
    kotlin
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

fn render_jni_c(api: &Api, package: &str) -> String {
    let jni_prefix = package.replace('.', "_");
    let mut jni_c = String::from("#include <jni.h>\n#include <stdbool.h>\n#include <stdint.h>\n#include <stddef.h>\n#include <stdlib.h>\n#include \"weaveffi.h\"\n\n");
    for m in &api.modules {
        for f in &m.functions {
            let jret = jni_ret_type(f.returns.as_ref());
            let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
            for p in &f.params {
                jparams.push(format!("{} {}", jni_param_type(&p.ty), p.name));
            }
            let jni_name = if has_enum_involvement(f) {
                format!("{}Jni", f.name)
            } else {
                f.name.clone()
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

            let c_sym = c_symbol_name(&m.name, &f.name);
            let mut call_args: Vec<String> = Vec::new();
            for p in &f.params {
                build_c_call_args(&mut call_args, &p.name, &p.ty, &m.name);
            }

            let needs_out_len = matches!(f.returns, Some(TypeRef::Bytes) | Some(TypeRef::List(_)));
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
                    &m.name,
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
    for m in &api.modules {
        for s in &m.structs {
            render_jni_struct(&mut jni_c, &m.name, s, &jni_prefix);
        }
    }
    jni_c
}

fn write_param_acquire(out: &mut String, name: &str, ty: &TypeRef) {
    match ty {
        TypeRef::StringUtf8 => {
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
        TypeRef::Bytes => {
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
        TypeRef::StringUtf8 => {
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
        TypeRef::Bytes => {
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {}
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
        TypeRef::I64 | TypeRef::Handle => "int64_t",
        TypeRef::F64 => "double",
        TypeRef::Bool => "jboolean",
        TypeRef::StringUtf8 => "const char*",
        _ => "void*",
    }
}

fn map_elem_c_call_cast(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => "(const int32_t*)",
        TypeRef::U32 => "(const uint32_t*)",
        TypeRef::I64 | TypeRef::Handle => "(const int64_t*)",
        TypeRef::F64 => "(const double*)",
        TypeRef::Bool => "(const bool*)",
        TypeRef::StringUtf8 => "(const char* const*)",
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
    if matches!(key, TypeRef::StringUtf8) {
        let _ = writeln!(
            out,
            "    jstring* {n}_jk = (jstring*)malloc((size_t){n}_len * sizeof(jstring));",
            n = name
        );
    }
    if matches!(val, TypeRef::StringUtf8) {
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => {
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
        TypeRef::StringUtf8 => {
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
        TypeRef::I64 | TypeRef::Handle => {
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
        TypeRef::StringUtf8 => {
            args.push(format!("(const uint8_t*){n}_chars", n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Bytes => {
            args.push(format!("(const uint8_t*){n}_elems", n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Bool => args.push(format!("(bool)({} == JNI_TRUE)", name)),
        TypeRef::I32 => args.push(format!("(int32_t){}", name)),
        TypeRef::U32 => args.push(format!("(uint32_t){}", name)),
        TypeRef::I64 => args.push(format!("(int64_t){}", name)),
        TypeRef::F64 => args.push(format!("(double){}", name)),
        TypeRef::Handle => args.push(format!("(weaveffi_handle_t){}", name)),
        TypeRef::Struct(sname) => {
            args.push(format!(
                "(const weaveffi_{}_{}*)(intptr_t){}",
                module, sname, name
            ));
        }
        TypeRef::Enum(_) => args.push(format!("(int32_t){}", name)),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                args.push(format!("(const uint8_t*){n}_chars", n = name));
                args.push(format!("(size_t){n}_len", n = name));
            }
            TypeRef::Bytes => {
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
                TypeRef::Handle => {
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
        TypeRef::StringUtf8 => {
            let _ = writeln!(jni_c, "    const char* rv = {}({}, &err);", c_sym, args_str);
            write_error_check(jni_c, returns);
            let _ = writeln!(jni_c, "    jstring out = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");");
            let _ = writeln!(jni_c, "    weaveffi_free_string(rv);");
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return out;");
        }
        TypeRef::Bytes => {
            let _ = writeln!(
                jni_c,
                "    const uint8_t* rv = {}({}, &out_len, &err);",
                c_sym, args_str
            );
            write_error_check(jni_c, returns);
            let _ = writeln!(
                jni_c,
                "    jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"
            );
            let _ = writeln!(jni_c, "    if (out && rv) {{ (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }}");
            let _ = writeln!(
                jni_c,
                "    weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"
            );
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
            let c_ty = format!("weaveffi_{}_{}", module, name);
            let _ = writeln!(jni_c, "    {}* rv = {}({}, &err);", c_ty, c_sym, args_str);
            write_error_check(jni_c, returns);
            release_jni_resources(jni_c, params);
            let _ = writeln!(jni_c, "    return (jlong)(intptr_t)rv;");
        }
        TypeRef::Optional(inner) => {
            write_optional_return(jni_c, inner, c_sym, &args_str, returns, params, module);
        }
        TypeRef::List(inner) => {
            write_list_return(jni_c, inner, c_sym, &args_str, returns, params);
        }
        TypeRef::Map(k, v) => {
            write_map_return(jni_c, k, v, c_sym, &args_str, returns, params);
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
        TypeRef::StringUtf8 => {
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
        TypeRef::StringUtf8 => {
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => {
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
    let _ = writeln!(
        out,
        "        jclass exClass = (*env)->FindClass(env, \"java/lang/RuntimeException\");"
    );
    let _ = writeln!(
        out,
        "        const char* msg = err.message ? err.message : \"WeaveFFI error\";"
    );
    let _ = writeln!(out, "        (*env)->ThrowNew(env, exClass, msg);");
    let _ = writeln!(out, "        weaveffi_error_clear(&err);");
    let _ = writeln!(out, "        {}", jni_default_return(ret_type));
    let _ = writeln!(out, "    }}");
}

fn release_jni_resources(out: &mut String, params: &[weaveffi_ir::ir::Param]) {
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                    n = p.name
                );
            }
            TypeRef::Bytes => {
                let _ = writeln!(
                    out,
                    "    (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                    n = p.name
                );
            }
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::StringUtf8 => {
                    let _ = writeln!(
                        out,
                        "    if ({n} != NULL) {{ (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars); }}",
                        n = p.name
                    );
                }
                TypeRef::Bytes => {
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
                TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
    if matches!(key, TypeRef::StringUtf8) {
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
    if matches!(val, TypeRef::StringUtf8) {
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
        TypeRef::Struct(name) | TypeRef::Enum(name) => name.clone(),
        other => kotlin_type(other),
    }
}

fn render_kotlin_struct(out: &mut String, s: &StructDef) {
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "class {} internal constructor(private var handle: Long) : java.io.Closeable {{",
        s.name
    );
    let _ = writeln!(out, "    companion object {{");
    let _ = writeln!(out, "        init {{ System.loadLibrary(\"weaveffi\") }}");
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
                let _ = writeln!(
                    out,
                    "    val {}: {} get() = {}(nativeGet{}(handle))",
                    f.name, kt_type, name, pascal
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
            TypeRef::StringUtf8 => {
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
            TypeRef::Bytes => {
                let _ = writeln!(out, "    size_t out_len = 0;");
                let _ = writeln!(
                    out,
                    "    const uint8_t* rv = {}((const {}*)(intptr_t)handle, &out_len);",
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
                let _ = writeln!(
                    out,
                    "    weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"
                );
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
                let _ = writeln!(
                    out,
                    "    const weaveffi_{}_{}* rv = {}((const {}*)(intptr_t)handle);",
                    module_name, name, getter_c, prefix
                );
                let _ = writeln!(out, "    return (jlong)(intptr_t)rv;");
            }
            TypeRef::Optional(inner) => {
                write_struct_optional_getter(out, inner, &getter_c, &prefix);
            }
            TypeRef::List(inner) => {
                write_struct_list_getter(out, inner, &getter_c, &prefix);
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
        TypeRef::StringUtf8 => {
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
        TypeRef::Struct(_) | TypeRef::Handle => {
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
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
        TypeRef::StringUtf8 => {
            let _ = writeln!(
                out,
                "    (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                n = name
            );
        }
        TypeRef::Bytes => {
            let _ = writeln!(
                out,
                "    (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            );
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                let _ = writeln!(
                    out,
                    "    if ({n} != NULL) {{ (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars); }}",
                    n = name
                );
            }
            TypeRef::Bytes => {
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
            TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) => {
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
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules,
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
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            errors: None,
        }])
    }

    #[test]
    fn kotlin_struct_class_declaration() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("class Contact internal constructor(private var handle: Long) : java.io.Closeable {"),
            "missing struct class declaration: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_create() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("@JvmStatic external fun nativeCreate(name: String, age: Int): Long"),
            "missing nativeCreate: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_destroy() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("@JvmStatic external fun nativeDestroy(handle: Long)"),
            "missing nativeDestroy: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_getters() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi");
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
        let kt = render_kotlin(&api, "com.weaveffi");
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
        let kt = render_kotlin(&api, "com.weaveffi");
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
        let kt = render_kotlin(&api, "com.weaveffi");
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
        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("protected fun finalize()"),
            "missing finalize: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_loads_library() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi");
        let struct_section = kt.split("class Contact").nth(1).unwrap();
        assert!(
            struct_section.contains("System.loadLibrary(\"weaveffi\")"),
            "struct companion missing loadLibrary: {kt}"
        );
    }

    #[test]
    fn jni_struct_native_create() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi");
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
        let jni = render_jni_c(&api, "com.weaveffi");
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
        let jni = render_jni_c(&api, "com.weaveffi");
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
        let jni = render_jni_c(&api, "com.weaveffi");
        assert!(
            jni.contains("weaveffi_free_string(rv)"),
            "missing free_string in getter: {jni}"
        );
    }

    #[test]
    fn jni_struct_create_handles_string_param() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi");
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
        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("val data: ByteArray get() = nativeGetData(handle)"),
            "missing bytes property: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("val start: Point get() = Point(nativeGetStart(handle))"),
            "missing nested struct property: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
        assert!(
            kt.contains("contact: Long"),
            "missing struct param as Long: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
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
                    },
                    Param {
                        name: "contact_type".to_string(),
                        ty: TypeRef::Enum("ContactType".into()),
                    },
                ],
                returns: Some(TypeRef::Enum("ContactType".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                ],
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
            errors: None,
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let kt = render_kotlin(&api, "com.weaveffi");
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
            errors: None,
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let jni = render_jni_c(&api, "com.weaveffi");
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
                    },
                    Param {
                        name: "b".to_string(),
                        ty: TypeRef::I32,
                    },
                ],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
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
            jni.contains("Java_com_mycompany_ffi_WeaveFFI_add"),
            "missing custom JNI prefix: {jni}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
