/*
 * Kvstore consumer smoke test (C).
 *
 * Forward-declares the kvstore C ABI it needs and links against
 * libkvstore. Exercises the minimum lifecycle every language binding
 * must support: open store, put a value, get it back, delete it,
 * close the store. Prints "OK" and exits 0 on success; any assertion
 * failure prints a diagnostic and exits 1.
 */
#include <stdio.h>
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <string.h>

typedef struct weaveffi_error {
    int32_t code;
    const char* message;
} weaveffi_error;

typedef struct weaveffi_kv_Store weaveffi_kv_Store;
typedef struct weaveffi_kv_Entry weaveffi_kv_Entry;

weaveffi_kv_Store* weaveffi_kv_open_store(const char* path, weaveffi_error* out_err);
void weaveffi_kv_close_store(weaveffi_kv_Store* store, weaveffi_error* out_err);
bool weaveffi_kv_put(
    weaveffi_kv_Store* store, const char* key,
    const uint8_t* value, size_t value_len,
    int32_t kind, const int64_t* ttl_seconds, weaveffi_error* out_err);
weaveffi_kv_Entry* weaveffi_kv_get(weaveffi_kv_Store* store, const char* key, weaveffi_error* out_err);
const uint8_t* weaveffi_kv_Entry_get_value(const weaveffi_kv_Entry* ptr, size_t* out_len);
void weaveffi_kv_Entry_destroy(weaveffi_kv_Entry* ptr);
bool weaveffi_kv_delete(weaveffi_kv_Store* store, const char* key, weaveffi_error* out_err);
void weaveffi_free_bytes(uint8_t* ptr, size_t len);

#define ASSERT(cond, msg) do { \
    if (!(cond)) { \
        fprintf(stderr, "assertion failed: %s (%s:%d)\n", msg, __FILE__, __LINE__); \
        return 1; \
    } \
} while (0)

int main(void) {
    weaveffi_error err = {0};
    weaveffi_kv_Store* store = weaveffi_kv_open_store("/tmp/kvstore-c-smoke", &err);
    ASSERT(err.code == 0, "open_store error");
    ASSERT(store != NULL, "open_store returned NULL");

    err = (weaveffi_error){0};
    const uint8_t value[] = {'h', 'e', 'l', 'l', 'o'};
    bool ok = weaveffi_kv_put(store, "greeting", value, 5, 1, NULL, &err);
    ASSERT(err.code == 0, "put error");
    ASSERT(ok, "put returned false");

    err = (weaveffi_error){0};
    weaveffi_kv_Entry* entry = weaveffi_kv_get(store, "greeting", &err);
    ASSERT(err.code == 0, "get error");
    ASSERT(entry != NULL, "get returned NULL");

    size_t len = 0;
    const uint8_t* got = weaveffi_kv_Entry_get_value(entry, &len);
    ASSERT(len == 5, "value length mismatch");
    ASSERT(memcmp(got, value, 5) == 0, "value bytes mismatch");
    weaveffi_free_bytes((uint8_t*)got, len);
    weaveffi_kv_Entry_destroy(entry);

    err = (weaveffi_error){0};
    bool deleted = weaveffi_kv_delete(store, "greeting", &err);
    ASSERT(err.code == 0, "delete error");
    ASSERT(deleted, "delete did not return true");

    err = (weaveffi_error){0};
    weaveffi_kv_close_store(store, &err);
    ASSERT(err.code == 0, "close_store error");

    printf("OK\n");
    return 0;
}
