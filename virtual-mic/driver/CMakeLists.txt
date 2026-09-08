# ReAI Vibe Board virtual microphone driver.
#
# Standalone CMake project; vendored libASPL (../libASPL) is built in-tree, so
# no system-wide libASPL installation is needed. Invoked by the SDK's build.rs
# when the `virtual-mic` feature is enabled on macOS.

cmake_minimum_required(VERSION 3.12.0)

project(ReAIVibeBoard CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(BUILD_TESTING OFF)

# universal binary so one bundle serves arm64 and x86_64 hosts
set(CMAKE_OSX_ARCHITECTURES "arm64;x86_64" CACHE STRING "" FORCE)

set(DRIVER_NAME "ReAIVibeBoard")
set(DRIVER_VERSION "0.1.0")
set(DRIVER_COPYRIGHT "Copyright (c) ReAI Team")
set(DRIVER_IDENTIFIER "com.reai.vibeboard.virtual-mic")
set(DRIVER_UID "D2E8FD96-5300-422D-8D19-C5853A9AB8B1")
set(DRIVER_ENTRYPOINT "ReAIVibeBoardEntryPoint")

# set to a non-empty string (e.g. "Developer ID Application: ...") to enable
# real code signing; empty leaves the linker's ad-hoc signature, which is
# enough to load (verified on macOS 26).
set(CODESIGN_ID "" CACHE STRING "Codesign ID")

message(STATUS "Driver name - ${DRIVER_NAME}")
message(STATUS "Driver version - ${DRIVER_VERSION}")
message(STATUS "Driver codesign ID - ${CODESIGN_ID}")

add_subdirectory(${CMAKE_CURRENT_LIST_DIR}/../libASPL
  ${CMAKE_CURRENT_BINARY_DIR}/libASPL-build)

add_library(${DRIVER_NAME} MODULE
  "Driver.cpp"
  )

add_dependencies(${DRIVER_NAME} libASPL)
target_include_directories(${DRIVER_NAME}
  PRIVATE ${CMAKE_CURRENT_LIST_DIR}/../libASPL/include
  )
target_link_libraries(${DRIVER_NAME}
  PRIVATE libASPL
  )

find_library(LIB_CoreFoundation CoreFoundation REQUIRED)
target_link_libraries(${DRIVER_NAME}
  PRIVATE ${LIB_CoreFoundation}
  )

set_target_properties(${DRIVER_NAME} PROPERTIES
  OUTPUT_NAME "${DRIVER_NAME}"
  BUNDLE true
  BUNDLE_EXTENSION "driver"
  PREFIX ""
  SUFFIX ""
  MACOSX_BUNDLE ON
  MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_SOURCE_DIR}/Info.plist.in"
  MACOSX_BUNDLE_BUNDLE_NAME "${DRIVER_NAME}"
  MACOSX_BUNDLE_BUNDLE_VERSION "${DRIVER_VERSION}"
  MACOSX_BUNDLE_COPYRIGHT "${DRIVER_COPYRIGHT}"
  MACOSX_BUNDLE_GUI_IDENTIFIER "${DRIVER_IDENTIFIER}"
  MACOSX_BUNDLE_SHORT_VERSION_STRING "${DRIVER_VERSION}"
  )

if(NOT CODESIGN_ID STREQUAL "")
  add_custom_command(
    TARGET ${DRIVER_NAME}
    POST_BUILD
    COMMENT
      "Signing ${DRIVER_NAME}.driver with ID ${CODESIGN_ID}"
    VERBATIM
    COMMAND
      codesign --force -s "${CODESIGN_ID}"
        "${CMAKE_CURRENT_BINARY_DIR}/${DRIVER_NAME}.driver"
    )
endif()
