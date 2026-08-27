//! Build-integration files: the `CMakeLists.txt` skeleton, the packaged
//! CMake importing prebuilt libraries, and the READMEs.
//!
//! CMake and Markdown are the only formats emitted here; neither is JSON or
//! XML, so the shared manifest escaping helpers don't apply. The only
//! interpolated values are the resolved package version and library name,
//! both validated identifiers rather than free-form user prose.

use weaveffi_core::package::PackageContext;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

/// Render a `CMakeLists.txt` that imports the bundled per-platform library as
/// the `weaveffi` target and links it into the `weaveffi_cpp` INTERFACE
/// library, selecting the right library for the host platform.
pub(crate) fn render_packaged_cmake(
    lib: &str,
    version: &str,
    cpp_std: &str,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "CMakeLists.txt");
    let body = r#"cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp VERSION @VERSION@)

# Select the prebuilt native library bundled for the host platform/arch.
if(APPLE)
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
    set(_wv_plat "darwin-arm64")
  else()
    set(_wv_plat "darwin-x64")
  endif()
  set(_wv_libfile "lib@LIB@.dylib")
elseif(WIN32)
  set(_wv_plat "windows-x64")
  set(_wv_libfile "@LIB@.dll")
else()
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(_wv_plat "linux-arm64")
  else()
    set(_wv_plat "linux-x64")
  endif()
  set(_wv_libfile "lib@LIB@.so")
endif()

add_library(weaveffi SHARED IMPORTED GLOBAL)
set_target_properties(weaveffi PROPERTIES
  IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/lib/${_wv_plat}/${_wv_libfile}")

add_library(weaveffi_cpp INTERFACE)
target_include_directories(weaveffi_cpp INTERFACE ${CMAKE_CURRENT_LIST_DIR}/include)
target_link_libraries(weaveffi_cpp INTERFACE weaveffi)
target_compile_features(weaveffi_cpp INTERFACE cxx_std_@STD@)
"#
    .replace("@VERSION@", version)
    .replace("@LIB@", lib)
    .replace("@STD@", cpp_std);
    format!("{prelude}{body}\n{trailer}")
}

/// README for a packaged C++ artifact bundling the header and per-platform libs.
pub(crate) fn render_packaged_readme(
    lib: &str,
    header_name: &str,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    out.push_str(&format!(
        "# {lib} (C++)

An idiomatic wrapper header (`include/{header_name}`) plus a prebuilt shared
library for each supported platform under `lib/<platform>/`.

## Use with CMake

```cmake
add_subdirectory(path/to/cpp)
target_link_libraries(your_app PRIVATE weaveffi_cpp)
```

`CMakeLists.txt` selects the right library for the host platform and links it
into the `weaveffi_cpp` interface target automatically.

## Bundled platforms

{platform_list}

"
    ));
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}

/// Render the source-layout `CMakeLists.txt`: an INTERFACE library adding the
/// generated header directory to the include path and linking `weaveffi`.
pub(crate) fn render_cmake(cpp_std: &str, version: &str, input_basename: &str) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(&format!(
        "cmake_minimum_required(VERSION 3.14)\n\
project(weaveffi_cpp VERSION {version})\n\
add_library(weaveffi_cpp INTERFACE)\n\
target_include_directories(weaveffi_cpp INTERFACE ${{CMAKE_CURRENT_SOURCE_DIR}})\n\
target_link_libraries(weaveffi_cpp INTERFACE weaveffi)\n\
target_compile_features(weaveffi_cpp INTERFACE cxx_std_{cpp_std})\n\n"
    ));
    out.push_str(&render_trailer(CommentStyle::Hash, "CMakeLists.txt"));
    out
}

/// README for the source layout: prerequisites and CMake usage.
pub(crate) fn render_readme(input_basename: &str) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    out.push_str(
        "# WeaveFFI C++ Bindings

## Prerequisites

- CMake 3.14+
- C++17 compiler
- The `weaveffi` static/shared library built from the Rust crate

## Usage with CMake

Add the generated `cpp/` directory as a subdirectory in your `CMakeLists.txt` and
link against `weaveffi_cpp`:

```cmake
add_subdirectory(path/to/generated/cpp)
add_executable(myapp main.cpp)
target_link_libraries(myapp weaveffi_cpp)
```

The `weaveffi_cpp` target is an INTERFACE library that:

- Adds the generated header directory to your include path
- Links against the `weaveffi` library
- Requires C++17

Then include the header in your code:

```cpp
#include \"weaveffi.hpp\"
```

",
    );
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}
