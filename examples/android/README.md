# Android End-to-End Smoke Test

A minimal Kotlin/JVM project that verifies the JNI binding declarations
matching the calculator and contacts C ABI compile under `kotlinc`. We
intentionally do not run on-device in CI; this only proves the binding
code is well-formed.

## Prerequisites

- `kotlinc` 1.9+ (or full Android Studio install for the real Android workflow)

## Smoke compile

```bash
kotlinc examples/android/src/main/kotlin/com/weaveffi/example/Main.kt \
    -include-runtime -d /tmp/weaveffi_android_smoke.jar
```

The full Android tutorial in `docs/src/tutorials/android.md` walks
through producing an `.aar` and integrating with an Android app module.
