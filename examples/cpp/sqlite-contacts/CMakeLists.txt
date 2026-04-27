cmake_minimum_required(VERSION 3.16)
project(weaveffi_cpp_sqlite_contacts CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Location of the Rust cdylib produced by `cargo build -p sqlite-contacts`.
# Override with -DSQLITE_CONTACTS_LIB_DIR=/path/to/dir (e.g. target/release) if needed.
if(NOT DEFINED SQLITE_CONTACTS_LIB_DIR)
    set(SQLITE_CONTACTS_LIB_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/debug")
endif()

if(APPLE)
    set(WEAVEFFI_IMPORTED_LOCATION "${SQLITE_CONTACTS_LIB_DIR}/libsqlite_contacts.dylib")
elseif(WIN32)
    set(WEAVEFFI_IMPORTED_LOCATION "${SQLITE_CONTACTS_LIB_DIR}/sqlite_contacts.dll")
else()
    set(WEAVEFFI_IMPORTED_LOCATION "${SQLITE_CONTACTS_LIB_DIR}/libsqlite_contacts.so")
endif()

# The generated weaveffi_cpp INTERFACE target transitively links against a
# library named `weaveffi`. Our sqlite-contacts cdylib is shipped as
# `libsqlite_contacts.*`, so expose it to CMake under the expected name via an
# IMPORTED target.
add_library(weaveffi SHARED IMPORTED GLOBAL)
set_target_properties(weaveffi PROPERTIES
    IMPORTED_LOCATION "${WEAVEFFI_IMPORTED_LOCATION}"
    IMPORTED_NO_SONAME TRUE
)
if(WIN32)
    set_target_properties(weaveffi PROPERTIES
        IMPORTED_IMPLIB "${SQLITE_CONTACTS_LIB_DIR}/sqlite_contacts.dll.lib"
    )
endif()

add_subdirectory(../../../generated/cpp ../weaveffi_cpp_build)

add_executable(sqlite_contacts main.cpp iter_contacts.cpp)
target_link_libraries(sqlite_contacts PRIVATE weaveffi_cpp ${CMAKE_DL_LIBS})

# On macOS, embed an rpath so the executable finds libsqlite_contacts.dylib
# without needing DYLD_LIBRARY_PATH. On Linux we still rely on LD_LIBRARY_PATH
# (see README).
if(APPLE)
    set_target_properties(sqlite_contacts PROPERTIES
        BUILD_RPATH "${SQLITE_CONTACTS_LIB_DIR}"
    )
endif()
