//! Packaging scaffolds for a distributable C artifact: the `CMakeLists.txt`
//! that exposes the bundled per-platform library as an imported target, and
//! the README describing the layout.

use crate::package::PackageContext;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

/// Render a `CMakeLists.txt` that exposes the bundled per-platform library as
/// an `IMPORTED` target (`<lib>::<lib>`) with the header's include directory,
/// selecting the right library for the host platform and architecture.
pub(crate) fn render_packaged_cmake(lib: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash);
    let trailer = render_trailer(CommentStyle::Hash, "CMakeLists.txt");
    let body = r#"cmake_minimum_required(VERSION 3.15)
project(@LIB@ C)

# Select the prebuilt library bundled for the host platform and architecture.
if(APPLE)
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
    set(_@LIB@_platform "darwin-arm64")
  else()
    set(_@LIB@_platform "darwin-x64")
  endif()
  set(_@LIB@_libfile "lib@LIB@.dylib")
elseif(WIN32)
  set(_@LIB@_platform "windows-x64")
  set(_@LIB@_libfile "@LIB@.dll")
else()
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(_@LIB@_platform "linux-arm64")
  else()
    set(_@LIB@_platform "linux-x64")
  endif()
  set(_@LIB@_libfile "lib@LIB@.so")
endif()

add_library(@LIB@ SHARED IMPORTED GLOBAL)
set_target_properties(@LIB@ PROPERTIES
  IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/lib/${_@LIB@_platform}/${_@LIB@_libfile}"
  INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_LIST_DIR}/include")
if(WIN32)
  set_target_properties(@LIB@ PROPERTIES
    IMPORTED_IMPLIB "${CMAKE_CURRENT_LIST_DIR}/lib/${_@LIB@_platform}/@LIB@.dll.lib")
endif()
add_library(@LIB@::@LIB@ ALIAS @LIB@)
"#
    .replace("@LIB@", lib);
    format!("{prelude}{body}\n{trailer}")
}

/// README for a packaged C artifact bundling the header and per-platform libs.
pub(crate) fn render_packaged_readme(lib: &str, header_name: &str, ctx: &PackageContext) -> String {
    let prelude = render_prelude(CommentStyle::Xml);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {lib} (C)

The stable C ABI header (`include/{header_name}`) plus a prebuilt shared library
for each supported platform under `lib/<platform>/`.

## Use with CMake

```cmake
add_subdirectory(path/to/{lib})
target_link_libraries(your_app PRIVATE {lib}::{lib})
```

`CMakeLists.txt` selects the right library for the host platform and exposes the
include directory automatically.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}
