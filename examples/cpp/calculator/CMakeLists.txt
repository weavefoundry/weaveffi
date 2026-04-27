cmake_minimum_required(VERSION 3.16)
project(weaveffi_cpp_calculator CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Location of the Rust cdylib produced by `cargo build -p calculator`.
# Override with -DCALCULATOR_LIB_DIR=/path/to/dir (e.g. target/release) if needed.
if(NOT DEFINED CALCULATOR_LIB_DIR)
    set(CALCULATOR_LIB_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/debug")
endif()

if(APPLE)
    set(WEAVEFFI_IMPORTED_LOCATION "${CALCULATOR_LIB_DIR}/libcalculator.dylib")
elseif(WIN32)
    set(WEAVEFFI_IMPORTED_LOCATION "${CALCULATOR_LIB_DIR}/calculator.dll")
else()
    set(WEAVEFFI_IMPORTED_LOCATION "${CALCULATOR_LIB_DIR}/libcalculator.so")
endif()

# The generated weaveffi_cpp INTERFACE target transitively links against a
# library named `weaveffi`. Our calculator cdylib is shipped as `libcalculator.*`,
# so expose it to CMake under the expected name via an IMPORTED target.
add_library(weaveffi SHARED IMPORTED GLOBAL)
set_target_properties(weaveffi PROPERTIES
    IMPORTED_LOCATION "${WEAVEFFI_IMPORTED_LOCATION}"
    IMPORTED_NO_SONAME TRUE
)
if(WIN32)
    set_target_properties(weaveffi PROPERTIES
        IMPORTED_IMPLIB "${CALCULATOR_LIB_DIR}/calculator.dll.lib"
    )
endif()

add_subdirectory(../../../generated/cpp ../weaveffi_cpp_build)

add_executable(calculator main.cpp)
target_link_libraries(calculator PRIVATE weaveffi_cpp ${CMAKE_DL_LIBS})

# On macOS, embed an rpath so the executable finds libcalculator.dylib without
# needing DYLD_LIBRARY_PATH. On Linux we still rely on LD_LIBRARY_PATH (see README).
if(APPLE)
    set_target_properties(calculator PROPERTIES
        BUILD_RPATH "${CALCULATOR_LIB_DIR}"
    )
endif()
