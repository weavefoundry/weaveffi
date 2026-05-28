# 03 — Stop Writing JNI (Single Post)

The pain-point post. Anyone who has shipped a native library to Android
has felt this. Lead with the relatable groan, deliver the relief.

---

## Hook

> Last year I wrote 230 lines of JNI to expose 4 functions to Android.
>
> Last week I wrote this:

**Image:** Two-panel split, before/after style. Use a red-tinted theme
for the JNI side and a green-tinted theme for the YAML side. Or just
"before / after" header text.

---

### Before — `Java_com_example_Contacts.c` (excerpt)

```c
JNIEXPORT jlong JNICALL
Java_com_example_Contacts_createContact(
    JNIEnv* env, jclass cls,
    jstring jname, jstring jemail, jint jtype)
{
    const char* name  = (*env)->GetStringUTFChars(env, jname,  NULL);
    const char* email = jemail
        ? (*env)->GetStringUTFChars(env, jemail, NULL)
        : NULL;

    weaveffi_error err = {0};
    uint64_t h = weaveffi_contacts_create_contact(name, email, jtype, &err);

    (*env)->ReleaseStringUTFChars(env, jname,  name);
    if (email) (*env)->ReleaseStringUTFChars(env, jemail, email);

    if (err.code) {
        jclass ex = (*env)->FindClass(env, "com/example/ContactException");
        (*env)->ThrowNew(env, ex, err.message);
        weaveffi_error_clear(&err);
        return 0;
    }
    return (jlong) h;
}
// ... and one of these per exported function, forever.
```

---

### After — `contacts.yml`

```yaml
- name: create_contact
  params:
    - { name: name,  type: string }
    - { name: email, type: "string?" }
    - { name: type,  type: ContactType }
  return: handle
```

---

## Body / outro

> WeaveFFI generates the JNI shim, the Kotlin wrapper, the Gradle
> skeleton, and the C ABI declaration. From one IDL.
>
> It also generates Swift, TypeScript, Python, Dart, C#, Go, Ruby,
> C, C++, and WASM bindings. Same file.
>
> `cargo install weaveffi-cli`

---

## Why this works

- **Visceral.** The JNI panel is *long on purpose*. Every developer who
  has touched JNI knows what's coming.
- **Asymmetric.** Compressing a wall of C into 6 lines of YAML is the
  whole demo.
- **Concrete numbers.** "230 lines" and "4 functions" are bait — people
  will reply with their own war stories. Engage them.

---

## Variations

Same template, different language:

- **N-API / Node addon:** Swap the JNI block for the napi setup +
  `napi_get_value_string_utf8` + `napi_create_string_utf8` round trip.
- **ctypes / Python:** Swap for a `ctypes.CDLL` + `restype` + `argtypes`
  + manual `Optional` handling block.
- **P/Invoke / .NET:** Swap for `[DllImport]` + `MarshalAs` annotations
  per parameter.

Each variation is a separate post. Pick the one whose audience you're
trying to reach this week.

---

## Alt text

"Two-panel comparison. Left panel: ~25 lines of dense JNI C code with
`Java_com_example_Contacts_createContact`, manual string conversions,
error handling, and exception throwing. Right panel: a 6-line YAML
function declaration with three typed parameters and a return type."
