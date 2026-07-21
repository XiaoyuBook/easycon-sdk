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

if(NOT WIN32 OR NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "Phase 3 native bridge supports Windows x64 only")
endif()

if(NOT MSVC)
  message(FATAL_ERROR "Phase 3 native bridge requires the MSVC-compatible ABI")
endif()

find_package(OpenCV 4.12.0 EXACT CONFIG REQUIRED COMPONENTS core imgcodecs imgproc)
find_package(ZLIB REQUIRED)

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

if(EASYCON_ENABLE_ASAN OR EASYCON_BUILD_FUZZERS)
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

if(EASYCON_BUILD_FUZZERS)
  easycon_native_find_compiler_runtime(
    EASYCON_FUZZER_RUNTIME
    clang_rt.fuzzer-x86_64.lib
  )
endif()

function(easycon_native_enable_asan target)
  target_compile_options(${target} PRIVATE -fsanitize=address)
  # Static vcpkg dependencies are built without MSVC STL ASan annotations.
  # Keep their ABI consistent while retaining instrumentation for owned code.
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
endfunction()

function(easycon_native_configure_owned_target target)
  target_compile_features(${target} PRIVATE cxx_std_20)
  set_target_properties(
    ${target}
    PROPERTIES
      CXX_EXTENSIONS OFF
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
    if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
      message(FATAL_ERROR "EASYCON_ENABLE_UBSAN requires clang-cl")
    endif()
    # LLVM's Windows standalone UBSan runtime uses /MT. Trap mode preserves
    # full undefined-behavior instrumentation without violating the frozen
    # /MD ABI used by owned targets and static vcpkg dependencies.
    target_compile_options(
      ${target}
      PRIVATE
        -fsanitize=undefined
        -fsanitize-trap=undefined
    )
  endif()
endfunction()

add_subdirectory(native/bridge)
