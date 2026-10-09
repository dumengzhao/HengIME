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

# HENG_DEV=ON：dev 并行身份（hengdev），与已安装的正式版 heng 并存互不影响
# —— 对齐 Windows HENG_DEV 五锚点：addon id / IM 名称 / 用户数据 / 库名 全换 dev 组
option(HENG_DEV "构建 dev 并行身份（hengdev）" OFF)
if(HENG_DEV)
    set(HENG_TARGET hengdev)
    set(HENG_USER_DIR_DEF "${REPO_ROOT}/runtime-dev/rime-user") # 用户数据隔离
else()
    set(HENG_TARGET heng)
    set(HENG_USER_DIR_DEF "${REPO_ROOT}/runtime/rime-user")
endif()

add_library(${HENG_TARGET} MODULE src/hengengine.cpp)
target_link_libraries(${HENG_TARGET} Fcitx5::Core Fcitx5::Config Fcitx5::Utils heng_core)
target_include_directories(${HENG_TARGET} PRIVATE "${REPO_ROOT}/core/include")
set_target_properties(${HENG_TARGET} PROPERTIES PREFIX "lib" SUFFIX ".so"
    INSTALL_RPATH "${REPO_ROOT}/target/release")

# 运行时目录：shared 共享（只读预设），user 按 dev/正式 隔离
target_compile_definitions(${HENG_TARGET} PRIVATE
    HENG_SHARED_DIR="${REPO_ROOT}/runtime/rime-shared"
    HENG_USER_DIR="${HENG_USER_DIR_DEF}"
    $<$<BOOL:${HENG_DEV}>:HENG_DEV>)

# 安装路径：fcitx5 插件库 + 元数据（需要 sudo 装到系统目录）
include(GNUInstallDirs)
find_library(FCITX5_LIB_DIR fcitx5core PATHS /usr/lib/x86_64-linux-gnu PATH_SUFFIXES fcitx5)
set(FCITX5_ADDON_LIBDIR "/usr/lib/x86_64-linux-gnu/fcitx5"
    CACHE PATH "fcitx5 addon library dir")
set(FCITX5_ADDON_CONFDIR "/usr/share/fcitx5/addons" CACHE PATH "fcitx5 addon conf dir")
set(FCITX5_IM_CONFDIR "/usr/share/fcitx5/inputmethod" CACHE PATH "fcitx5 inputmethod conf dir")

install(TARGETS ${HENG_TARGET} LIBRARY DESTINATION "${FCITX5_ADDON_LIBDIR}")
install(FILES conf/addon.conf RENAME heng.conf DESTINATION "${FCITX5_ADDON_CONFDIR}")
install(FILES conf/im.conf RENAME heng.conf DESTINATION "${FCITX5_IM_CONFDIR}")
