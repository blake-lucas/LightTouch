cmake_minimum_required(VERSION 3.22.1)
project(inferno_jni C)

# The bridge between the app and the engine. The engine itself
# (libqemu-aarch64-softmmu.so) is not linked here: it is opened with dlopen at
# run time, so the app builds and starts without one and can say what is wrong.
add_library(inferno_jni SHARED inferno_jni.c)
target_compile_options(inferno_jni PRIVATE -O2 -Wall -Wextra -Wno-unused-parameter)
target_link_libraries(inferno_jni PRIVATE android log dl)
target_link_options(inferno_jni PRIVATE -Wl,-z,max-page-size=16384)
