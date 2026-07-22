cmake_minimum_required(VERSION 3.30)

project(easycon_native LANGUAGES CXX)

include(CTest)

option(EASYCON_BUILD_COMPONENT_TESTS "Build private native component tests" ON)
option(EASYCON_BUILD_FUZZERS "Build private native fuzz targets" OFF)
option(EASYCON_ENABLE_ASAN "Instrument owned native targets with AddressSanitizer" OFF)
option(EASYCON_ENABLE_UBSAN "Instrument owned native targets with UndefinedBehaviorSanitizer" OFF)
option(EASYCON_ENABLE_CLANG_TIDY "Create the fixed clang-tidy target" OFF)
option(EASYCON_ENABLE_MSVC_ANALYZE "Run MSVC code analysis for owned native targets" OFF)
option(EASYCON_NATIVE_RUST_BUILD "Configure the bridge for easycon-native-sys" OFF)
option(
  EASYCON_EXPERIMENTAL_MACOS
  "Enable the build-unverified Apple Silicon source boundary"
  OFF
)

if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "Phase 3 native bridge supports 64-bit targets only")
endif()

if(WIN32)
  if(NOT MSVC OR NOT CMAKE_SYSTEM_PROCESSOR MATCHES "^(AMD64|amd64|x86_64)$")
    message(FATAL_ERROR "Windows native bridge requires the MSVC-compatible x64 ABI")
  endif()
elseif(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  if(NOT CMAKE_SYSTEM_PROCESSOR MATCHES "^(x86_64|amd64|AMD64)$")
    message(FATAL_ERROR "Linux Vision candidate requires x64")
  endif()
  if(NOT CMAKE_CXX_COMPILER_ID MATCHES "^(Clang|GNU)$")
    message(FATAL_ERROR "Linux Vision candidate requires Clang or GCC")
  endif()
elseif(APPLE)
  if(NOT EASYCON_EXPERIMENTAL_MACOS)
    message(FATAL_ERROR "macOS is build-unverified; opt in with EASYCON_EXPERIMENTAL_MACOS=ON")
  endif()
  if(CMAKE_OSX_ARCHITECTURES AND NOT CMAKE_OSX_ARCHITECTURES STREQUAL "arm64")
    message(FATAL_ERROR "Intel and universal macOS outputs are not in the Phase 3 source candidate")
  endif()
  if(NOT CMAKE_SYSTEM_PROCESSOR MATCHES "^(arm64|aarch64)$" AND
     NOT CMAKE_OSX_ARCHITECTURES STREQUAL "arm64")
    message(FATAL_ERROR "The experimental macOS source boundary is Apple Silicon arm64 only")
  endif()
else()
  message(FATAL_ERROR "Phase 3 native bridge targets Windows x64, Linux x64, or experimental macOS arm64")
endif()

find_package(OpenCV 4.12.0 EXACT CONFIG REQUIRED COMPONENTS core imgcodecs imgproc videoio)
find_package(Tesseract CONFIG REQUIRED)
find_package(ZLIB REQUIRED)

if(EASYCON_BUILD_FUZZERS AND NOT WIN32)
  message(FATAL_ERROR "Phase 3 native fuzz packaging is a Windows-only gate")
endif()

function(easycon_native_find_compiler_runtime output_name runtime_name)
  execute_process(
    COMMAND ${CMAKE_CXX_COMPILER} --print-file-name=${runtime_name}
    RESULT_VARIABLE result
    OUTPUT_VARIABLE runtime_path
    OUTPUT_STRIP_TRAILING_WHITESPACE
  )
  if(NOT result EQUAL 0 OR runtime_path STREQUAL runtime_name OR NOT EXISTS "${runtime_path}")
    message(FATAL_ERROR "clang-cl runtime ${runtime_name} was not found")
  endif()
  set(${output_name} "${runtime_path}" PARENT_SCOPE)
endfunction()

if(WIN32 AND (EASYCON_ENABLE_ASAN OR EASYCON_BUILD_FUZZERS))
  if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
    message(FATAL_ERROR "ASan and fuzz targets require clang-cl")
  endif()
  easycon_native_find_compiler_runtime(
    EASYCON_ASAN_RUNTIME
    clang_rt.asan_dynamic-x86_64.lib
  )
  easycon_native_find_compiler_runtime(
    EASYCON_ASAN_THUNK
    clang_rt.asan_dynamic_runtime_thunk-x86_64.lib
  )
  easycon_native_find_compiler_runtime(
    EASYCON_ASAN_DLL
    clang_rt.asan_dynamic-x86_64.dll
  )
endif()

if(WIN32 AND EASYCON_BUILD_FUZZERS)
  easycon_native_find_compiler_runtime(
    EASYCON_FUZZER_RUNTIME
    clang_rt.fuzzer-x86_64.lib
  )
endif()

function(easycon_native_enable_asan target)
  if(WIN32)
    target_compile_options(${target} PRIVATE -fsanitize=address)
    # Static vcpkg dependencies are built without MSVC STL ASan annotations.
    target_compile_definitions(
      ${target}
      PRIVATE
        _DISABLE_STRING_ANNOTATION
        _DISABLE_VECTOR_ANNOTATION
    )
    target_link_libraries(${target} PRIVATE "${EASYCON_ASAN_RUNTIME}")
    target_link_options(
      ${target}
      PRIVATE
        /INCLUDE:__asan_seh_interceptor
        "/WHOLEARCHIVE:${EASYCON_ASAN_THUNK}"
        /INFERASANLIBS:NO
    )
    get_target_property(target_type ${target} TYPE)
    if(target_type STREQUAL "EXECUTABLE")
      add_custom_command(
        TARGET ${target}
        POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
          "${EASYCON_ASAN_DLL}"
          "$<TARGET_FILE_DIR:${target}>"
        VERBATIM
      )
    endif()
  else()
    target_compile_options(${target} PRIVATE -fsanitize=address -fno-omit-frame-pointer)
    target_link_options(${target} PRIVATE -fsanitize=address)
  endif()
endfunction()

function(easycon_native_configure_owned_target target)
  target_compile_features(${target} PRIVATE cxx_std_20)
  set_target_properties(
    ${target}
    PROPERTIES
      CXX_EXTENSIONS OFF
  )
  if(WIN32)
    set_target_properties(
      ${target}
      PROPERTIES
        MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>DLL"
    )
    target_compile_options(
      ${target}
      PRIVATE
        /W4
        /WX
        /permissive-
        /EHsc
        /Zc:__cplusplus
    )
  else()
    target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic -Werror)
  endif()

  if(EASYCON_ENABLE_MSVC_ANALYZE)
    if(NOT CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
      message(FATAL_ERROR "EASYCON_ENABLE_MSVC_ANALYZE requires cl.exe")
    endif()
    target_compile_options(${target} PRIVATE /analyze /analyze:external-)
  endif()

  if(EASYCON_ENABLE_ASAN)
    easycon_native_enable_asan(${target})
  endif()

  if(EASYCON_ENABLE_UBSAN)
    if(WIN32)
      if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
        message(FATAL_ERROR "Windows EASYCON_ENABLE_UBSAN requires clang-cl")
      endif()
      # Trap mode avoids mixing the /MT UBSan runtime with the frozen /MD ABI.
      target_compile_options(
        ${target}
        PRIVATE
          -fsanitize=undefined
          -fsanitize-trap=undefined
      )
    else()
      target_compile_options(${target} PRIVATE -fsanitize=undefined)
      target_link_options(${target} PRIVATE -fsanitize=undefined)
    endif()
  endif()
endfunction()

add_subdirectory(native/bridge)
