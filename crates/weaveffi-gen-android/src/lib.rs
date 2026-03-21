use anyhow::Result;
use camino::Utf8Path;
use std::fmt::Write as _;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, TypeRef};

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
        TypeRef::Struct(_) => todo!("struct codegen"),
        TypeRef::Enum(_) => todo!("enum codegen"),
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
        TypeRef::Struct(_) => todo!("struct codegen"),
        TypeRef::Enum(_) => todo!("enum codegen"),
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
        Some(TypeRef::Struct(_)) => todo!("struct codegen"),
        Some(TypeRef::Enum(_)) => todo!("enum codegen"),
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
        TypeRef::Struct(_) => todo!("struct codegen"),
        TypeRef::Enum(_) => todo!("enum codegen"),
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
        Some(TypeRef::Struct(_)) => todo!("struct codegen"),
        Some(TypeRef::Enum(_)) => todo!("enum codegen"),
    }
}

fn jni_cast_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 => "(jint)",
        TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => "(jlong)",
        TypeRef::F64 => "(jdouble)",
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
                match p.ty {
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
                    TypeRef::Struct(_) => todo!("struct codegen"),
                    TypeRef::Enum(_) => todo!("enum codegen"),
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
