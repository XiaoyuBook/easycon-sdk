set(EASYCON_NATIVE_BRIDGE_SOURCES
  src/common/bridge.cpp
  src/common/capture_common.cpp
  src/common/image_codec.cpp
  src/common/ocr.cpp
  src/common/vision_ops.cpp
)

if(WIN32)
  set(EASYCON_NATIVE_CAPTURE_PLATFORM_SOURCE src/platform/windows/capture.cpp)
  set(EASYCON_NATIVE_PLATFORM_LIBRARIES mf mfplat mfuuid ole32 oleaut32 strmiids)
elseif(CMAKE_SYSTEM_NAME STREQUAL "Linux")
  set(EASYCON_NATIVE_CAPTURE_PLATFORM_SOURCE src/platform/linux/capture.cpp)
  set(EASYCON_NATIVE_PLATFORM_LIBRARIES)
elseif(APPLE)
  set(EASYCON_NATIVE_CAPTURE_PLATFORM_SOURCE src/platform/macos/capture_unavailable.cpp)
  set(EASYCON_NATIVE_PLATFORM_LIBRARIES)
else()
  message(FATAL_ERROR "No native capture platform source selected")
endif()

list(APPEND EASYCON_NATIVE_BRIDGE_SOURCES ${EASYCON_NATIVE_CAPTURE_PLATFORM_SOURCE})

set(EASYCON_NATIVE_BRIDGE_LIBRARY_TYPE STATIC)
if(EASYCON_NATIVE_RUST_BUILD AND NOT WIN32)
  set(EASYCON_NATIVE_BRIDGE_LIBRARY_TYPE SHARED)
endif()

add_library(
  easycon_native_bridge
  ${EASYCON_NATIVE_BRIDGE_LIBRARY_TYPE}
  ${EASYCON_NATIVE_BRIDGE_SOURCES}
)
target_include_directories(
  easycon_native_bridge
  PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src/common
)
target_include_directories(easycon_native_bridge SYSTEM PRIVATE ${OpenCV_INCLUDE_DIRS})
target_link_libraries(
  easycon_native_bridge
  PRIVATE
    ${OpenCV_LIBS}
    Tesseract::libtesseract
    ZLIB::ZLIB
    ${EASYCON_NATIVE_PLATFORM_LIBRARIES}
)
set_target_properties(
  easycon_native_bridge
  PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
)
if(NOT WIN32 AND DEFINED VCPKG_TARGET_TRIPLET)
  set_target_properties(
    easycon_native_bridge
    PROPERTIES
      BUILD_RPATH "${CMAKE_BINARY_DIR}/vcpkg_installed/${VCPKG_TARGET_TRIPLET}/lib"
  )
endif()
easycon_native_configure_owned_target(easycon_native_bridge)

set(EASYCON_NATIVE_ANALYSIS_SOURCES
  ${CMAKE_CURRENT_SOURCE_DIR}/src/common/bridge.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/src/common/capture_common.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/src/common/image_codec.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/src/common/ocr.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/src/common/vision_ops.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/${EASYCON_NATIVE_CAPTURE_PLATFORM_SOURCE}
)

if(EASYCON_BUILD_COMPONENT_TESTS OR EASYCON_BUILD_FUZZERS)
  target_compile_definitions(easycon_native_bridge PRIVATE EASYCON_NATIVE_TESTING=1)
  add_library(easycon_native_bridge_test_support STATIC src/test_support.cpp)
  target_compile_definitions(easycon_native_bridge_test_support PRIVATE EASYCON_NATIVE_TESTING=1)
  target_include_directories(
    easycon_native_bridge_test_support
    PRIVATE
      ${CMAKE_CURRENT_SOURCE_DIR}/include
      ${CMAKE_CURRENT_SOURCE_DIR}/src/common
  )
  target_include_directories(easycon_native_bridge_test_support SYSTEM PRIVATE ${OpenCV_INCLUDE_DIRS})
  target_link_libraries(easycon_native_bridge_test_support PRIVATE easycon_native_bridge ${OpenCV_LIBS})
  easycon_native_configure_owned_target(easycon_native_bridge_test_support)
  list(APPEND EASYCON_NATIVE_ANALYSIS_SOURCES ${CMAKE_CURRENT_SOURCE_DIR}/src/test_support.cpp)
endif()

if(EASYCON_BUILD_COMPONENT_TESTS)
  find_package(Python3 REQUIRED COMPONENTS Interpreter)
  foreach(
    fixture
    IN ITEMS
      bgr-2x2.bmp.hex
      bgr-2x2.png.hex
      expected-bgr.hex
      gray-alpha-1x1.png.hex
      palette-1x1.png.hex
  )
    configure_file(
      ${PROJECT_SOURCE_DIR}/spec/fixtures/vision/codec/${fixture}
      ${CMAKE_CURRENT_BINARY_DIR}/fixtures/codec/${fixture}
      COPYONLY
    )
  endforeach()

  configure_file(
    ${PROJECT_SOURCE_DIR}/spec/fixtures/vision/ocr/easycon-gray.hex
    ${CMAKE_CURRENT_BINARY_DIR}/fixtures/ocr/easycon-gray.hex
    COPYONLY
  )

  foreach(
    fixture
    IN ITEMS
      template-search-gray.hex
      template-target-gray.hex
      edge-search-gray.hex
      edge-target-gray.hex
      edge-search-xy-gray.hex
      edge-target-xy-gray.hex
      edge-search-laplacian-gray.hex
      edge-target-laplacian-gray.hex
      hsv-bgr-5x3.hex
  )
    configure_file(
      ${PROJECT_SOURCE_DIR}/spec/fixtures/vision/operations/${fixture}
      ${CMAKE_CURRENT_BINARY_DIR}/fixtures/operations/${fixture}
      COPYONLY
    )
  endforeach()

  add_executable(easycon_native_bridge_tests tests/bridge_component_tests.cpp)
  target_compile_definitions(easycon_native_bridge_tests PRIVATE EASYCON_NATIVE_TESTING=1)
  target_include_directories(
    easycon_native_bridge_tests
    PRIVATE
      ${CMAKE_CURRENT_SOURCE_DIR}/include
  )
  target_link_libraries(
    easycon_native_bridge_tests
    PRIVATE
      easycon_native_bridge_test_support
      easycon_native_bridge
  )
  easycon_native_configure_owned_target(easycon_native_bridge_tests)
  add_test(NAME easycon_native_bridge_components COMMAND easycon_native_bridge_tests)
  set_tests_properties(easycon_native_bridge_components PROPERTIES TIMEOUT 30)
  list(APPEND EASYCON_NATIVE_ANALYSIS_SOURCES ${CMAKE_CURRENT_SOURCE_DIR}/tests/bridge_component_tests.cpp)

  add_executable(easycon_native_ocr_tests tests/ocr_component_tests.cpp)
  target_compile_definitions(easycon_native_ocr_tests PRIVATE EASYCON_NATIVE_TESTING=1)
  target_include_directories(
    easycon_native_ocr_tests
    PRIVATE
      ${CMAKE_CURRENT_SOURCE_DIR}/include
  )
  target_link_libraries(
    easycon_native_ocr_tests
    PRIVATE
      easycon_native_bridge_test_support
      easycon_native_bridge
  )
  easycon_native_configure_owned_target(easycon_native_ocr_tests)
  add_test(
    NAME easycon_native_ocr_components
    COMMAND
      ${Python3_EXECUTABLE}
      ${PROJECT_SOURCE_DIR}/tools/run_vision_ocr_component.py
      --manifest
      ${PROJECT_SOURCE_DIR}/spec/fixtures/vision/ocr-model.json
      --executable
      $<TARGET_FILE:easycon_native_ocr_tests>
  )
  set_tests_properties(
    easycon_native_ocr_components
    PROPERTIES
      LABELS ocr-success
      TIMEOUT 60
  )
  list(APPEND EASYCON_NATIVE_ANALYSIS_SOURCES ${CMAKE_CURRENT_SOURCE_DIR}/tests/ocr_component_tests.cpp)
endif()

if(EASYCON_BUILD_FUZZERS)
  if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
    message(FATAL_ERROR "EASYCON_BUILD_FUZZERS requires Clang")
  endif()
  if(NOT WIN32)
    message(FATAL_ERROR "Phase 3 fuzz packaging is currently a Windows-only gate")
  endif()

  add_library(
    easycon_native_bridge_fuzz_target
    SHARED
      ${EASYCON_NATIVE_BRIDGE_SOURCES}
      src/test_support.cpp
      fuzz/fuzz_target.def
  )
  target_compile_definitions(
    easycon_native_bridge_fuzz_target
    PRIVATE
      EASYCON_NATIVE_TESTING=1
  )
  target_include_directories(
    easycon_native_bridge_fuzz_target
    PRIVATE
      ${CMAKE_CURRENT_SOURCE_DIR}/include
      ${CMAKE_CURRENT_SOURCE_DIR}/src/common
  )
  target_include_directories(
    easycon_native_bridge_fuzz_target
    SYSTEM PRIVATE
      ${OpenCV_INCLUDE_DIRS}
  )
  target_link_libraries(
    easycon_native_bridge_fuzz_target
    PRIVATE
      ${OpenCV_LIBS}
      Tesseract::libtesseract
      ZLIB::ZLIB
      ${EASYCON_NATIVE_PLATFORM_LIBRARIES}
  )
  easycon_native_configure_owned_target(easycon_native_bridge_fuzz_target)
  easycon_native_enable_asan(easycon_native_bridge_fuzz_target)

  add_executable(easycon_native_abi_fuzzer fuzz/abi_fuzzer.cpp)
  target_compile_definitions(easycon_native_abi_fuzzer PRIVATE EASYCON_NATIVE_TESTING=1)
  target_include_directories(easycon_native_abi_fuzzer PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
  target_link_libraries(
    easycon_native_abi_fuzzer
    PRIVATE
      easycon_native_bridge_fuzz_target
  )
  easycon_native_configure_owned_target(easycon_native_abi_fuzzer)
  # The official LLVM Windows libFuzzer runtime is built with /MT. The SUT
  # remains /MD in a private DLL, and all ABI allocations are released there.
  set_target_properties(
    easycon_native_abi_fuzzer
    PROPERTIES
      MSVC_RUNTIME_LIBRARY "MultiThreaded"
  )
  easycon_native_enable_asan(easycon_native_abi_fuzzer)
  target_compile_options(easycon_native_abi_fuzzer PRIVATE -fsanitize=fuzzer-no-link)
  target_link_options(
    easycon_native_abi_fuzzer
    PRIVATE
      "/WHOLEARCHIVE:${EASYCON_FUZZER_RUNTIME}"
  )
  list(APPEND EASYCON_NATIVE_ANALYSIS_SOURCES ${CMAKE_CURRENT_SOURCE_DIR}/fuzz/abi_fuzzer.cpp)

  add_custom_target(
    easycon_native_fuzzers
    DEPENDS
      easycon_native_abi_fuzzer
      easycon_native_bridge_fuzz_target
  )
  foreach(seed IN ITEMS capture.txt cv.txt std.txt unknown.txt)
    configure_file(
      ${CMAKE_CURRENT_SOURCE_DIR}/fuzz/seeds/abi/${seed}
      ${CMAKE_BINARY_DIR}/fuzz-corpus/abi/${seed}
      COPYONLY
    )
  endforeach()
  add_test(
    NAME easycon_native_abi_fuzz_seed_replay
    COMMAND easycon_native_abi_fuzzer -runs=128 ${CMAKE_BINARY_DIR}/fuzz-corpus/abi
  )
  set_tests_properties(
    easycon_native_abi_fuzz_seed_replay
    PROPERTIES
      LABELS fuzz-seed-replay
      TIMEOUT 30
  )
endif()

if(EASYCON_ENABLE_CLANG_TIDY)
  find_program(EASYCON_CLANG_TIDY_EXE NAMES clang-tidy REQUIRED)
  add_custom_target(
    easycon_native_clang_tidy
    COMMAND
      ${EASYCON_CLANG_TIDY_EXE}
      -p=${CMAKE_BINARY_DIR}
      --config-file=${PROJECT_SOURCE_DIR}/.clang-tidy
      --warnings-as-errors=*
      ${EASYCON_NATIVE_ANALYSIS_SOURCES}
    WORKING_DIRECTORY ${PROJECT_SOURCE_DIR}
    VERBATIM
  )
endif()
