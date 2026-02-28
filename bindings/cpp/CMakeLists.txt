cmake_minimum_required(VERSION 3.15)
project(synapse-cpp VERSION 0.1.0 LANGUAGES CXX)

# Header-only interface library
add_library(synapse INTERFACE)
add_library(synapse::synapse ALIAS synapse)

target_include_directories(synapse INTERFACE
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<INSTALL_INTERFACE:include>
)

target_compile_features(synapse INTERFACE cxx_std_17)

# Linux requires -lrt for POSIX shared memory
if(UNIX AND NOT APPLE)
    target_link_libraries(synapse INTERFACE rt)
endif()

# Install rules
include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

install(TARGETS synapse
    EXPORT synapse-targets
)

install(EXPORT synapse-targets
    FILE synapse-targets.cmake
    NAMESPACE synapse::
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/synapse
)

install(DIRECTORY include/
    DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
)

configure_package_config_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/synapse-config.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/synapse-config.cmake"
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/synapse
)

write_basic_package_version_file(
    "${CMAKE_CURRENT_BINARY_DIR}/synapse-config-version.cmake"
    VERSION ${PROJECT_VERSION}
    COMPATIBILITY SameMajorVersion
)

install(FILES
    "${CMAKE_CURRENT_BINARY_DIR}/synapse-config.cmake"
    "${CMAKE_CURRENT_BINARY_DIR}/synapse-config-version.cmake"
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/synapse
)
