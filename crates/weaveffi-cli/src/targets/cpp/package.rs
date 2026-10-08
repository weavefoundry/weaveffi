//! Build-integration files: the source-layout `CMakeLists.txt`, the packaged
//! CMake importing prebuilt libraries, and the READMEs.
//!
//! CMake and Markdown are the only formats emitted here; neither is JSON or
//! XML, so the shared manifest escaping helpers don't apply. The only
//! interpolated values are identity names (C identifiers), the version, and
//! the configured C++ standard.

use crate::package::PackageContext;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

/// The names the build files and READMEs interpolate.
pub(crate) struct CmakeNames<'a> {
    /// The package identity.
    pub(crate) identity: &'a Identity,
    /// The C++ namespace.
    pub(crate) namespace: &'a str,
    /// The wrapper header's file name.
    pub(crate) header: &'a str,
    /// The C++ standard (`17`).
    pub(crate) standard: &'a str,
}

impl CmakeNames<'_> {
    /// Replace the `@NAME@` placeholders shared by the CMake templates.
    fn fill(&self, template: &str) -> String {
        let id = self.identity;
        template
            .replace("@LIBRARY@", &id.library)
            .replace("@VERSION@", &id.version)
            .replace("@ENV@", &id.library_env_var())
            .replace("@STD@", self.standard)
            .replace("@HEADER@", self.header)
            .replace("@NAMESPACE@", self.namespace)
    }
}

/// Render the source-layout `CMakeLists.txt`: an INTERFACE library
/// `{library}_cpp` (alias `{library}::cpp`) that adds the generated headers
/// to the include path and links the producer library, by the path in the
/// `{PREFIX}_LIBRARY` cache variable or environment variable when set, by
/// the name `{library}` otherwise.
pub(crate) fn render_cmake(names: &CmakeNames<'_>) -> String {
    let body = r#"cmake_minimum_required(VERSION 3.14)
project(@LIBRARY@_cpp VERSION @VERSION@ LANGUAGES CXX)

# The producer library to link: a full path from the @ENV@ cache or
# environment variable, or else the library name `@LIBRARY@` (a CMake target
# of that name, or lib@LIBRARY@ on the linker search path).
set(@ENV@ "$ENV{@ENV@}" CACHE FILEPATH "Path to the @LIBRARY@ native library")

add_library(@LIBRARY@_cpp INTERFACE)
add_library(@LIBRARY@::cpp ALIAS @LIBRARY@_cpp)
target_include_directories(@LIBRARY@_cpp INTERFACE ${CMAKE_CURRENT_SOURCE_DIR})
target_compile_features(@LIBRARY@_cpp INTERFACE cxx_std_@STD@)
if(@ENV@)
  target_link_libraries(@LIBRARY@_cpp INTERFACE "${@ENV@}")
else()
  target_link_libraries(@LIBRARY@_cpp INTERFACE @LIBRARY@)
endif()
"#;
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::Hash),
        names.fill(body),
        render_trailer(CommentStyle::Hash, "CMakeLists.txt")
    )
}

/// Render the packaged `CMakeLists.txt`, which imports the bundled library
/// for the host platform as `{library}` and links it into the
/// `{library}_cpp` INTERFACE library. `lib` is the bundled binaries' base
/// file name.
pub(crate) fn render_packaged_cmake(names: &CmakeNames<'_>, lib: &str) -> String {
    let body = r#"cmake_minimum_required(VERSION 3.14)
project(@LIBRARY@_cpp VERSION @VERSION@ LANGUAGES CXX)

# Select the prebuilt native library bundled for the host platform/arch.
if(APPLE)
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
    set(_plat "darwin-arm64")
  else()
    set(_plat "darwin-x64")
  endif()
  set(_libfile "lib@LIB@.dylib")
elseif(WIN32)
  set(_plat "windows-x64")
  set(_libfile "@LIB@.dll")
else()
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(_plat "linux-arm64")
  else()
    set(_plat "linux-x64")
  endif()
  set(_libfile "lib@LIB@.so")
endif()

if(NOT TARGET @LIBRARY@)
  add_library(@LIBRARY@ SHARED IMPORTED GLOBAL)
  set_target_properties(@LIBRARY@ PROPERTIES
    IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/lib/${_plat}/${_libfile}")
  if(WIN32)
    set_target_properties(@LIBRARY@ PROPERTIES
      IMPORTED_IMPLIB "${CMAKE_CURRENT_LIST_DIR}/lib/${_plat}/@LIB@.dll.lib")
  endif()
endif()

add_library(@LIBRARY@_cpp INTERFACE)
add_library(@LIBRARY@::cpp ALIAS @LIBRARY@_cpp)
target_include_directories(@LIBRARY@_cpp INTERFACE ${CMAKE_CURRENT_LIST_DIR}/include)
target_link_libraries(@LIBRARY@_cpp INTERFACE @LIBRARY@)
target_compile_features(@LIBRARY@_cpp INTERFACE cxx_std_@STD@)
"#;
    format!(
        "{}{}\n{}",
        render_prelude(CommentStyle::Hash),
        names.fill(body).replace("@LIB@", lib),
        render_trailer(CommentStyle::Hash, "CMakeLists.txt")
    )
}

/// README for a packaged C++ artifact bundling the headers and the desktop
/// per-platform libraries the packaged CMake can select among.
pub(crate) fn render_packaged_readme(names: &CmakeNames<'_>, ctx: &PackageContext) -> String {
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .filter(|p| p.is_desktop())
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let body = r#"# @LIBRARY@ (C++)

A header-only C++@STD@ wrapper (`include/@HEADER@`), the C header it
includes, and a prebuilt shared library for each supported platform under
`lib/<platform>/`.

## Use with CMake

```cmake
add_subdirectory(path/to/cpp)
target_link_libraries(your_app PRIVATE @LIBRARY@::cpp)
```

`CMakeLists.txt` selects the right library for the host platform and links
it into the `@LIBRARY@::cpp` interface target.

## Bundled platforms

"#;
    format!(
        "{}{}{}\n\n{}",
        render_prelude(CommentStyle::Xml),
        names.fill(body),
        platforms.join("\n"),
        render_trailer(CommentStyle::Xml, "README.md")
    )
}

/// README for the source layout: prerequisites, CMake usage, and the
/// library-path override.
pub(crate) fn render_readme(names: &CmakeNames<'_>) -> String {
    let body = r#"# @LIBRARY@ C++ bindings

A header-only C++@STD@ wrapper over the `@LIBRARY@` C ABI. `@HEADER@`
includes the C header beside it; every declaration lives in
`namespace @NAMESPACE@`.

## Prerequisites

- CMake 3.14 or later
- A C++@STD@ compiler
- The `@LIBRARY@` native library (`lib@LIBRARY@.so`, `lib@LIBRARY@.dylib`, or
  `@LIBRARY@.dll`)

## Use with CMake

```cmake
add_subdirectory(path/to/cpp)
add_executable(app main.cpp)
target_link_libraries(app PRIVATE @LIBRARY@::cpp)
```

`@LIBRARY@::cpp` adds this directory to the include path, requires
C++@STD@, and links the native library: the file named by the `@ENV@`
CMake cache variable or environment variable when set, or else
`@LIBRARY@` by name.

```cpp
#include "@HEADER@"
```

The first free function, constructor, or static member you call checks
that the linked library matches these headers and throws
`@NAMESPACE@::LoadError` if it doesn't; call `@NAMESPACE@::check_library()`
at startup to check eagerly.

"#;
    format!(
        "{}{}{}",
        render_prelude(CommentStyle::Xml),
        names.fill(body),
        render_trailer(CommentStyle::Xml, "README.md")
    )
}
