cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp_example CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

add_executable(cpp_example main.cpp)
target_link_libraries(cpp_example PRIVATE ${CMAKE_DL_LIBS})
