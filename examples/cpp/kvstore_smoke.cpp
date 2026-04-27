// Kvstore consumer smoke test (C++).
//
// Loads KVSTORE_LIB at runtime via dlopen and exercises the minimum
// lifecycle every language binding must support: open store, put a
// value, get it back, delete it, close the store. Prints "OK" and
// exits 0 on success; any assertion failure exits 1.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dlfcn.h>

extern "C" {
struct weaveffi_error {
    int32_t code;
    const char* message;
};
}

static void* must_open(const char* path) {
    void* h = dlopen(path, RTLD_NOW | RTLD_GLOBAL);
    if (!h) {
        std::fprintf(stderr, "dlopen(%s): %s\n", path, dlerror());
        std::exit(1);
    }
    return h;
}

template <typename Fn>
static Fn must_sym(void* lib, const char* name) {
    dlerror();
    void* p = dlsym(lib, name);
    const char* err = dlerror();
    if (err) {
        std::fprintf(stderr, "dlsym(%s): %s\n", name, err);
        std::exit(1);
    }
    return reinterpret_cast<Fn>(p);
}

#define ASSERT(cond, msg)                                                                          \
    do {                                                                                           \
        if (!(cond)) {                                                                             \
            std::fprintf(stderr, "assertion failed: %s (%s:%d)\n", msg, __FILE__, __LINE__);       \
            std::exit(1);                                                                          \
        }                                                                                          \
    } while (0)

int main() {
    const char* kv_path = std::getenv("KVSTORE_LIB");
    if (!kv_path) {
        std::fprintf(stderr, "KVSTORE_LIB must be set\n");
        return 1;
    }

    void* kv = must_open(kv_path);

    using OpenFn = void* (*)(const char*, weaveffi_error*);
    using CloseFn = void (*)(void*, weaveffi_error*);
    using PutFn = bool (*)(void*, const char*, const uint8_t*, size_t, int32_t,
                           const int64_t*, weaveffi_error*);
    using GetFn = void* (*)(void*, const char*, weaveffi_error*);
    using EntryGetValueFn = const uint8_t* (*)(const void*, size_t*);
    using EntryDestroyFn = void (*)(void*);
    using DeleteFn = bool (*)(void*, const char*, weaveffi_error*);
    using FreeBytesFn = void (*)(uint8_t*, size_t);

    auto open_store = must_sym<OpenFn>(kv, "weaveffi_kv_open_store");
    auto close_store = must_sym<CloseFn>(kv, "weaveffi_kv_close_store");
    auto put = must_sym<PutFn>(kv, "weaveffi_kv_put");
    auto get = must_sym<GetFn>(kv, "weaveffi_kv_get");
    auto entry_value = must_sym<EntryGetValueFn>(kv, "weaveffi_kv_Entry_get_value");
    auto entry_destroy = must_sym<EntryDestroyFn>(kv, "weaveffi_kv_Entry_destroy");
    auto del = must_sym<DeleteFn>(kv, "weaveffi_kv_delete");
    auto free_bytes = must_sym<FreeBytesFn>(kv, "weaveffi_free_bytes");

    weaveffi_error err{};
    void* store = open_store("/tmp/kvstore-cpp-smoke", &err);
    ASSERT(err.code == 0, "open_store error");
    ASSERT(store != nullptr, "open_store returned null");

    err = {};
    const uint8_t value[] = {'h', 'e', 'l', 'l', 'o'};
    bool ok = put(store, "greeting", value, 5, 1, nullptr, &err);
    ASSERT(err.code == 0, "put error");
    ASSERT(ok, "put returned false");

    err = {};
    void* entry = get(store, "greeting", &err);
    ASSERT(err.code == 0, "get error");
    ASSERT(entry != nullptr, "get returned null");

    size_t len = 0;
    const uint8_t* got = entry_value(entry, &len);
    ASSERT(len == 5, "value length mismatch");
    ASSERT(std::memcmp(got, value, 5) == 0, "value bytes mismatch");
    free_bytes(const_cast<uint8_t*>(got), len);
    entry_destroy(entry);

    err = {};
    bool deleted = del(store, "greeting", &err);
    ASSERT(err.code == 0, "delete error");
    ASSERT(deleted, "delete did not return true");

    err = {};
    close_store(store, &err);
    ASSERT(err.code == 0, "close_store error");

    std::printf("OK\n");
    dlclose(kv);
    return 0;
}
