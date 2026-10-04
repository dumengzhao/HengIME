cmake_minimum_required(VERSION 3.20)
project(heng-fcitx5 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(Fcitx5Core REQUIRED)
find_package(Fcitx5Utils REQUIRED)

# heng_core cdylib：仓库根 target/ 下的构建产物（先 cargo build --release）
get_filename_component(REPO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
add_library(heng_core SHARED IMPORTED)
set_target_properties(heng_core PROPERTIES
    IMPORTED_LOCATION "${REPO_ROOT}/target/release/libheng_core.so")

add_library(heng MODULE src/hengengine.cpp)
target_link_libraries(heng Fcitx5::Core Fcitx5::Config Fcitx5::Utils heng_core)
target_include_directories(heng PRIVATE "${REPO_ROOT}/core/include")
set_target_properties(heng PROPERTIES PREFIX "lib" SUFFIX ".so"
    INSTALL_RPATH "${REPO_ROOT}/target/release")

# 试用版：运行时目录硬编码到仓库 runtime（生产版改读配置）
target_compile_definitions(heng PRIVATE
    HENG_SHARED_DIR="${REPO_ROOT}/runtime/rime-shared"
    HENG_USER_DIR="${REPO_ROOT}/runtime/rime-user")

# 安装路径：fcitx5 插件库 + 元数据（需要 sudo 装到系统目录）
include(GNUInstallDirs)
find_library(FCITX5_LIB_DIR fcitx5core PATHS /usr/lib/x86_64-linux-gnu PATH_SUFFIXES fcitx5)
set(FCITX5_ADDON_LIBDIR "/usr/lib/x86_64-linux-gnu/fcitx5"
    CACHE PATH "fcitx5 addon library dir")
set(FCITX5_ADDON_CONFDIR "/usr/share/fcitx5/addons" CACHE PATH "fcitx5 addon conf dir")
set(FCITX5_IM_CONFDIR "/usr/share/fcitx5/inputmethod" CACHE PATH "fcitx5 inputmethod conf dir")

install(TARGETS heng LIBRARY DESTINATION "${FCITX5_ADDON_LIBDIR}")
install(FILES conf/addon.conf RENAME heng.conf DESTINATION "${FCITX5_ADDON_CONFDIR}")
install(FILES conf/im.conf RENAME heng.conf DESTINATION "${FCITX5_IM_CONFDIR}")
