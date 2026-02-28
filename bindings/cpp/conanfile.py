from conan import ConanFile
from conan.tools.files import copy
import os


class SynapseConan(ConanFile):
    name = "synapse"
    version = "0.1.0"
    description = "Cross-language runtime bridge via shared memory + lock-free ring buffers"
    homepage = "https://github.com/zeron-G/Synapse"
    url = "https://github.com/zeron-G/Synapse"
    license = "MIT"
    topics = ("shared-memory", "ipc", "ring-buffer", "cross-language", "zero-copy")
    package_type = "header-library"
    no_copy_source = True

    def source(self):
        pass  # header is bundled with the recipe

    def package(self):
        copy(self, "*.h",
             src=os.path.join(self.source_folder, "include"),
             dst=os.path.join(self.package_folder, "include"))

    def package_info(self):
        self.cpp_info.bindirs = []
        self.cpp_info.libdirs = []
        self.cpp_info.set_property("cmake_file_name", "synapse")
        self.cpp_info.set_property("cmake_target_name", "synapse::synapse")
        # Linux requires -lrt for POSIX shared memory
        if self.settings.os == "Linux":
            self.cpp_info.system_libs = ["rt"]

    def package_id(self):
        self.info.clear()  # header-only: no binary variation
