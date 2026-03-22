use anyhow::Result;
use camino::Utf8Path;
use std::fmt::Write as _;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, StructDef, TypeRef};

pub struct AndroidGenerator;

impl Generator for AndroidGenerator {
    fn name(&self) -> &'static str {
        "android"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("android");
        std::fs::create_dir_all(&dir)?;

        std::fs::write(
            dir.join("settings.gradle"),
            "rootProject.name = 'weaveffi'\n",
        )?;
        std::fs::write(dir.join("build.gradle"), BUILD_GRADLE)?;

        let src_dir = dir.join("src/main/kotlin/com/weaveffi");
        std::fs::create_dir_all(&src_dir)?;
        let kotlin = render_kotlin(api);
        std::fs::write(src_dir.join("WeaveFFI.kt"), kotlin)?;

        let jni_dir = dir.join("src/main/cpp");
        std::fs::create_dir_all(&jni_dir)?;
        std::fs::write(jni_dir.join("CMakeLists.txt"), CMAKE)?;
        let jni_c = render_jni_c(api);
        std::fs::write(jni_dir.join("weaveffi_jni.c"), jni_c)?;

        Ok(())
    }
}

const BUILD_GRADLE: &str = r#"plugins {
    id 'com.android.library'
    id 'org.jetbrains.kotlin.android' version '1.9.22' apply false
}

android {
    namespace 'com.weaveffi'
    compileSdk 34
    defaultConfig { minSdk 24 }
}
"#;

const CMAKE: &str = r#"cmake_minimum_required(VERSION 3.22)
project(weaveffi)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
"#;

fn kotlin_type(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 => "Int",
        TypeRef::U32 => "Long",
        TypeRef::I64 => "Long",
        TypeRef::F64 => "Double",
        TypeRef::Bool => "Boolean",
        TypeRef::StringUtf8 => "String",
        TypeRef::Bytes => "ByteArray",
        TypeRef::Handle => "Long",
        TypeRef::Struct(_) => "Long",
        TypeRef::Enum(_) => todo!("enum codegen"),
        TypeRef::Optional(_) => todo!("optional codegen"),
        TypeRef::List(_) => todo!("list codegen"),
    }
}

fn jni_param_type(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 => "jint",
        TypeRef::U32 => "jlong",
        TypeRef::I64 | TypeRef::Handle => "jlong",
        TypeRef::F64 => "jdouble",
        TypeRef::Bool => "jboolean",
        TypeRef::StringUtf8 => "jstring",
        TypeRef::Bytes => "jbyteArray",
        TypeRef::Struct(_) => "jlong",
        TypeRef::Enum(_) => todo!("enum codegen"),
        TypeRef::Optional(_) => todo!("optional codegen"),
        TypeRef::List(_) => todo!("list codegen"),
    }
}

fn jni_ret_type(t: Option<&TypeRef>) -> &'static str {
    match t {
        None => "void",
        Some(TypeRef::I32) => "jint",
        Some(TypeRef::U32) => "jlong",
        Some(TypeRef::I64 | TypeRef::Handle) => "jlong",
        Some(TypeRef::F64) => "jdouble",
        Some(TypeRef::Bool) => "jboolean",
        Some(TypeRef::StringUtf8) => "jstring",
        Some(TypeRef::Bytes) => "jbyteArray",
        Some(TypeRef::Struct(_)) => "jlong",
        Some(TypeRef::Enum(_)) => todo!("enum codegen"),
        Some(TypeRef::Optional(_)) => todo!("optional codegen"),
        Some(TypeRef::List(_)) => todo!("list codegen"),
    }
}

fn c_type_for_return(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 => "int32_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::I64 => "int64_t",
        TypeRef::F64 => "double",
        TypeRef::Bool => "bool",
        TypeRef::Handle => "weaveffi_handle_t",
        TypeRef::StringUtf8 => "const char*",
        TypeRef::Bytes => "const uint8_t*",
        TypeRef::Struct(_) => "void*",
        TypeRef::Enum(_) => todo!("enum codegen"),
        TypeRef::Optional(_) => todo!("optional codegen"),
        TypeRef::List(_) => todo!("list codegen"),
    }
}

fn jni_default_return(t: Option<&TypeRef>) -> &'static str {
    match t {
        None => "",
        Some(TypeRef::I32) => "return 0;",
        Some(TypeRef::U32 | TypeRef::I64 | TypeRef::Handle) => "return 0;",
        Some(TypeRef::F64) => "return 0.0;",
        Some(TypeRef::Bool) => "return JNI_FALSE;",
        Some(TypeRef::StringUtf8) => "return NULL;",
        Some(TypeRef::Bytes) => "return NULL;",
        Some(TypeRef::Struct(_)) => "return 0;",
        Some(TypeRef::Enum(_)) => todo!("enum codegen"),
        Some(TypeRef::Optional(_)) => todo!("optional codegen"),
        Some(TypeRef::List(_)) => todo!("list codegen"),
    }
}

fn jni_cast_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 => "(jint)",
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => "(jlong)",
        TypeRef::F64 => "(jdouble)",
        TypeRef::Struct(_) => "(jlong)(intptr_t)",
        _ => "",
    }
}

fn render_kotlin(api: &Api) -> String {
    let mut kotlin = String::from("package com.weaveffi\n\nclass WeaveFFI {\n    companion object {\n        init { System.loadLibrary(\"weaveffi\") }\n\n");
    for m in &api.modules {
        for f in &m.functions {
            let params_sig: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, kotlin_type(&p.ty)))
                .collect();
            let ret = f.returns.as_ref().map(kotlin_type).unwrap_or("Unit");
            let _ = writeln!(
                kotlin,
                "        @JvmStatic external fun {}({}): {}",
                f.name,
                params_sig.join(", "),
                ret
            );
        }
    }
    kotlin.push_str("    }\n}\n");
    for m in &api.modules {
        for s in &m.structs {
            render_kotlin_struct(&mut kotlin, s);
        }
    }
    kotlin
}

fn render_jni_c(api: &Api) -> String {
    let mut jni_c = String::from("#include <jni.h>\n#include <stdbool.h>\n#include <stdint.h>\n#include <stddef.h>\n#include \"weaveffi.h\"\n\n");
    for m in &api.modules {
        for f in &m.functions {
            let jret = jni_ret_type(f.returns.as_ref());
            let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
            for p in &f.params {
                jparams.push(format!("{} {}", jni_param_type(&p.ty), p.name));
            }
            let _ = writeln!(
                jni_c,
                "JNIEXPORT {} JNICALL Java_com_weaveffi_WeaveFFI_{}({}) {{",
                jret,
                f.name,
                jparams.join(", ")
            );
            let _ = writeln!(jni_c, "    weaveffi_error err = {{0, NULL}};");

            // Acquire JNI resources
            for p in &f.params {
                match p.ty {
                    TypeRef::StringUtf8 => {
                        let _ = writeln!(jni_c, "    const char* {n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);", n = p.name);
                        let _ = writeln!(
                            jni_c,
                            "    jsize {n}_len = (*env)->GetStringUTFLength(env, {n});",
                            n = p.name
                        );
                    }
                    TypeRef::Bytes => {
                        let _ = writeln!(jni_c, "    jboolean {n}_is_copy = 0;", n = p.name);
                        let _ = writeln!(jni_c, "    jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, &{n}_is_copy);", n = p.name);
                        let _ = writeln!(
                            jni_c,
                            "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                            n = p.name
                        );
                    }
                    _ => {}
                }
            }

            // Build C call args
            let c_sym = c_symbol_name(&m.name, &f.name);
            let mut call_args: Vec<String> = Vec::new();
            for p in &f.params {
                match &p.ty {
                    TypeRef::StringUtf8 => {
                        call_args.push(format!("(const uint8_t*){n}_chars", n = p.name));
                        call_args.push(format!("(size_t){n}_len", n = p.name));
                    }
                    TypeRef::Bytes => {
                        call_args.push(format!("(const uint8_t*){n}_elems", n = p.name));
                        call_args.push(format!("(size_t){n}_len", n = p.name));
                    }
                    TypeRef::Bool => call_args.push(format!("(bool)({} == JNI_TRUE)", p.name)),
                    TypeRef::I32 => call_args.push(format!("(int32_t){}", p.name)),
                    TypeRef::U32 => call_args.push(format!("(uint32_t){}", p.name)),
                    TypeRef::I64 => call_args.push(format!("(int64_t){}", p.name)),
                    TypeRef::F64 => call_args.push(format!("(double){}", p.name)),
                    TypeRef::Handle => call_args.push(format!("(weaveffi_handle_t){}", p.name)),
                    TypeRef::Struct(name) => {
                        call_args.push(format!(
                            "(const weaveffi_{}_{}*)(intptr_t){}",
                            m.name, name, p.name
                        ));
                    }
                    TypeRef::Enum(_) => todo!("enum codegen"),
                    TypeRef::Optional(_) => todo!("optional codegen"),
                    TypeRef::List(_) => todo!("list codegen"),
                }
            }

            let needs_out_len = matches!(f.returns, Some(TypeRef::Bytes));
            if needs_out_len {
                let _ = writeln!(jni_c, "    size_t out_len = 0;");
            }

            // Call and handle return
            if let Some(ret_type) = f.returns.as_ref() {
                match ret_type {
                    TypeRef::StringUtf8 => {
                        let _ = writeln!(
                            jni_c,
                            "    const char* rv = {}({}, &err);",
                            c_sym,
                            call_args.join(", ")
                        );
                        write_error_check(&mut jni_c, f.returns.as_ref());
                        let _ = writeln!(jni_c, "    jstring out = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");");
                        let _ = writeln!(jni_c, "    weaveffi_free_string(rv);");
                        release_jni_resources(&mut jni_c, &f.params);
                        let _ = writeln!(jni_c, "    return out;");
                    }
                    TypeRef::Bytes => {
                        let mut args = call_args.clone();
                        args.push("&out_len".into());
                        let _ = writeln!(
                            jni_c,
                            "    const uint8_t* rv = {}({}, &err);",
                            c_sym,
                            args.join(", ")
                        );
                        write_error_check(&mut jni_c, f.returns.as_ref());
                        let _ = writeln!(
                            jni_c,
                            "    jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"
                        );
                        let _ = writeln!(jni_c, "    if (out && rv) {{ (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }}");
                        let _ = writeln!(
                            jni_c,
                            "    weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"
                        );
                        release_jni_resources(&mut jni_c, &f.params);
                        let _ = writeln!(jni_c, "    return out;");
                    }
                    TypeRef::Bool => {
                        let _ = writeln!(
                            jni_c,
                            "    bool rv = {}({}, &err);",
                            c_sym,
                            call_args.join(", ")
                        );
                        write_error_check(&mut jni_c, f.returns.as_ref());
                        release_jni_resources(&mut jni_c, &f.params);
                        let _ = writeln!(jni_c, "    return rv ? JNI_TRUE : JNI_FALSE;");
                    }
                    TypeRef::Struct(name) => {
                        let c_ty = format!("weaveffi_{}_{}", m.name, name);
                        let _ = writeln!(
                            jni_c,
                            "    {}* rv = {}({}, &err);",
                            c_ty,
                            c_sym,
                            call_args.join(", ")
                        );
                        write_error_check(&mut jni_c, f.returns.as_ref());
                        release_jni_resources(&mut jni_c, &f.params);
                        let _ = writeln!(jni_c, "    return (jlong)(intptr_t)rv;");
                    }
                    ret_type => {
                        let c_ty = c_type_for_return(ret_type);
                        let jcast = jni_cast_for(ret_type);
                        let _ = writeln!(
                            jni_c,
                            "    {} rv = {}({}, &err);",
                            c_ty,
                            c_sym,
                            call_args.join(", ")
                        );
                        write_error_check(&mut jni_c, f.returns.as_ref());
                        release_jni_resources(&mut jni_c, &f.params);
                        let _ = writeln!(jni_c, "    return {} rv;", jcast);
                    }
                }
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
            render_jni_struct(&mut jni_c, &m.name, s);
        }
    }
    jni_c
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
        match p.ty {
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
            _ => {}
        }
    }
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
        TypeRef::Struct(name) => name.clone(),
        other => kotlin_type(other).to_string(),
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

fn render_jni_struct(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    // nativeCreate
    {
        let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
        for f in &s.fields {
            jparams.push(format!("{} {}", jni_param_type(&f.ty), f.name));
        }
        let _ = writeln!(
            out,
            "JNIEXPORT jlong JNICALL Java_com_weaveffi_{}_nativeCreate({}) {{",
            s.name,
            jparams.join(", ")
        );
        let _ = writeln!(out, "    weaveffi_error err = {{0, NULL}};");

        for f in &s.fields {
            match f.ty {
                TypeRef::StringUtf8 => {
                    let _ = writeln!(
                        out,
                        "    const char* {n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);",
                        n = f.name
                    );
                    let _ = writeln!(
                        out,
                        "    jsize {n}_len = (*env)->GetStringUTFLength(env, {n});",
                        n = f.name
                    );
                }
                TypeRef::Bytes => {
                    let _ = writeln!(out, "    jboolean {n}_is_copy = 0;", n = f.name);
                    let _ = writeln!(
                        out,
                        "    jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, &{n}_is_copy);",
                        n = f.name
                    );
                    let _ = writeln!(
                        out,
                        "    jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                        n = f.name
                    );
                }
                _ => {}
            }
        }

        let mut call_args: Vec<String> = Vec::new();
        for f in &s.fields {
            match &f.ty {
                TypeRef::StringUtf8 => {
                    call_args.push(format!("(const uint8_t*){n}_chars", n = f.name));
                    call_args.push(format!("(size_t){n}_len", n = f.name));
                }
                TypeRef::Bytes => {
                    call_args.push(format!("(const uint8_t*){n}_elems", n = f.name));
                    call_args.push(format!("(size_t){n}_len", n = f.name));
                }
                TypeRef::Bool => {
                    call_args.push(format!("(bool)({} == JNI_TRUE)", f.name));
                }
                TypeRef::I32 => call_args.push(format!("(int32_t){}", f.name)),
                TypeRef::U32 => call_args.push(format!("(uint32_t){}", f.name)),
                TypeRef::I64 => call_args.push(format!("(int64_t){}", f.name)),
                TypeRef::F64 => call_args.push(format!("(double){}", f.name)),
                TypeRef::Handle => call_args.push(format!("(weaveffi_handle_t){}", f.name)),
                TypeRef::Struct(name) => {
                    call_args.push(format!(
                        "(const weaveffi_{}_{}*)(intptr_t){}",
                        module_name, name, f.name
                    ));
                }
                TypeRef::Enum(_) | TypeRef::Optional(_) | TypeRef::List(_) => {}
            }
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
            match f.ty {
                TypeRef::StringUtf8 => {
                    let _ = writeln!(
                        out,
                        "    (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                        n = f.name
                    );
                }
                TypeRef::Bytes => {
                    let _ = writeln!(
                        out,
                        "    (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                        n = f.name
                    );
                }
                _ => {}
            }
        }

        let _ = writeln!(out, "    return (jlong)(intptr_t)rv;");
        let _ = writeln!(out, "}}\n");
    }

    // nativeDestroy
    {
        let _ = writeln!(
            out,
            "JNIEXPORT void JNICALL Java_com_weaveffi_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
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
            "JNIEXPORT {} JNICALL Java_com_weaveffi_{}_nativeGet{}(JNIEnv* env, jclass clazz, jlong handle) {{",
            jret, s.name, pascal
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

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Function, Module, Param, StructDef, StructField};

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
        let kt = render_kotlin(&api);
        assert!(
            kt.contains("class Contact internal constructor(private var handle: Long) : java.io.Closeable {"),
            "missing struct class declaration: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_create() {
        let api = make_struct_api();
        let kt = render_kotlin(&api);
        assert!(
            kt.contains("@JvmStatic external fun nativeCreate(name: String, age: Int): Long"),
            "missing nativeCreate: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_destroy() {
        let api = make_struct_api();
        let kt = render_kotlin(&api);
        assert!(
            kt.contains("@JvmStatic external fun nativeDestroy(handle: Long)"),
            "missing nativeDestroy: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_getters() {
        let api = make_struct_api();
        let kt = render_kotlin(&api);
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
        let kt = render_kotlin(&api);
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
        let kt = render_kotlin(&api);
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
        let kt = render_kotlin(&api);
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
        let kt = render_kotlin(&api);
        assert!(
            kt.contains("protected fun finalize()"),
            "missing finalize: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_loads_library() {
        let api = make_struct_api();
        let kt = render_kotlin(&api);
        let struct_section = kt.split("class Contact").nth(1).unwrap();
        assert!(
            struct_section.contains("System.loadLibrary(\"weaveffi\")"),
            "struct companion missing loadLibrary: {kt}"
        );
    }

    #[test]
    fn jni_struct_native_create() {
        let api = make_struct_api();
        let jni = render_jni_c(&api);
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
        let jni = render_jni_c(&api);
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
        let jni = render_jni_c(&api);
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
        let jni = render_jni_c(&api);
        assert!(
            jni.contains("weaveffi_free_string(rv)"),
            "missing free_string in getter: {jni}"
        );
    }

    #[test]
    fn jni_struct_create_handles_string_param() {
        let api = make_struct_api();
        let jni = render_jni_c(&api);
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
        let jni = render_jni_c(&api);
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

        let kt = render_kotlin(&api);
        assert!(
            kt.contains("val data: ByteArray get() = nativeGetData(handle)"),
            "missing bytes property: {kt}"
        );

        let jni = render_jni_c(&api);
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

        let kt = render_kotlin(&api);
        assert!(
            kt.contains("val start: Point get() = Point(nativeGetStart(handle))"),
            "missing nested struct property: {kt}"
        );

        let jni = render_jni_c(&api);
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

        let kt = render_kotlin(&api);
        assert!(
            kt.contains("contact: Long"),
            "missing struct param as Long: {kt}"
        );

        let jni = render_jni_c(&api);
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

        let jni = render_jni_c(&api);
        assert!(
            jni.contains("weaveffi_contacts_Contact* rv"),
            "missing struct return type: {jni}"
        );
        assert!(
            jni.contains("return (jlong)(intptr_t)rv;"),
            "missing struct return cast: {jni}"
        );
    }
}
