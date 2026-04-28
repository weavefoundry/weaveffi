// End-to-end consumer test for the C++ binding consumers.
//
// Loads the calculator and contacts cdylibs at runtime via dlopen and
// exercises a representative slice of the C ABI: add, create_contact,
// list_contacts, delete_contact. Prints "OK" and exits 0 on success;
// any assertion failure prints a diagnostic and exits 1.

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

using weaveffi_handle_t = uint64_t;

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
    const char* calc_path = std::getenv("WEAVEFFI_LIB");
    const char* contacts_path = std::getenv("CONTACTS_LIB");
    if (!calc_path || !contacts_path) {
        std::fprintf(stderr, "WEAVEFFI_LIB and CONTACTS_LIB must be set\n");
        return 1;
    }

    void* calc = must_open(calc_path);
    void* contacts = must_open(contacts_path);

    using AddFn = int32_t (*)(int32_t, int32_t, weaveffi_error*);
    using CreateFn = weaveffi_handle_t (*)(const char*, const char*, const char*, int32_t,
                                           weaveffi_error*);
    using ListFn = void** (*)(size_t*, weaveffi_error*);
    using GetIdFn = int64_t (*)(const void*);
    using ListFreeFn = void (*)(void**, size_t);
    using DeleteFn = int32_t (*)(weaveffi_handle_t, weaveffi_error*);
    using CountFn = int32_t (*)(weaveffi_error*);

    auto add = must_sym<AddFn>(calc, "weaveffi_calculator_add");
    auto create = must_sym<CreateFn>(contacts, "weaveffi_contacts_create_contact");
    auto list = must_sym<ListFn>(contacts, "weaveffi_contacts_list_contacts");
    auto get_id = must_sym<GetIdFn>(contacts, "weaveffi_contacts_Contact_get_id");
    auto list_free = must_sym<ListFreeFn>(contacts, "weaveffi_contacts_Contact_list_free");
    auto del = must_sym<DeleteFn>(contacts, "weaveffi_contacts_delete_contact");
    auto count = must_sym<CountFn>(contacts, "weaveffi_contacts_count_contacts");

    weaveffi_error err{};
    int32_t sum = add(2, 3, &err);
    ASSERT(err.code == 0, "calculator_add error");
    ASSERT(sum == 5, "calculator_add(2,3) != 5");

    err = {};
    weaveffi_handle_t h = create("Alice", "Smith", "alice@example.com", 0, &err);
    ASSERT(err.code == 0, "create_contact error");
    ASSERT(h != 0, "create_contact returned 0");

    err = {};
    size_t len = 0;
    void** items = list(&len, &err);
    ASSERT(err.code == 0, "list_contacts error");
    ASSERT(len == 1, "list_contacts length != 1");
    ASSERT(items != nullptr, "list_contacts null");
    ASSERT(get_id(items[0]) == static_cast<int64_t>(h), "id mismatch");
    list_free(items, len);

    err = {};
    int32_t deleted = del(h, &err);
    ASSERT(err.code == 0, "delete_contact error");
    ASSERT(deleted == 1, "delete_contact did not return 1");

    err = {};
    ASSERT(count(&err) == 0, "store not empty after cleanup");

    std::printf("OK\n");
    dlclose(contacts);
    dlclose(calc);
    return 0;
}
