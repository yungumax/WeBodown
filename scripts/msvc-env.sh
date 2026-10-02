# WeBodown 的便携 MSVC 链接环境（Git Bash 里 source 本文件后即可 cargo build）。
#
# 本机没有安装 VS Build Tools：链接用 rust-lld（复制成 lld-link.exe），
# CRT 用「VC14 x64 静态库 + Windows SDK 的 libucrt/系统库」组合
# （/nodefaultlib:msvcrt + libcmt + libucrt + libvcruntime，等价 /MT）。
# 所有文件都在本仓库 .lldbin/ 下，删目录即完全回滚。

WBO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WBO_WIN="$(cygpath -m "$WBO_ROOT")"
SDK="C:\\Program Files (x86)\\Windows Kits\\10"
SDK_VER="10.0.26100.0"
VC_LIB="$WBO_WIN\\.lldbin\\x64_extract\\Program Files\\Microsoft Visual Studio 14.0\\VC\\lib\\amd64"
VC_INC="$WBO_WIN\\.lldbin\\headers_extract\\Program Files\\Microsoft Visual Studio 14.0\\VC\\include"
# 现代工具集（VC 14.44 vsix）：cl.exe + mspdb140.dll + 语言资源，cc-rs / rc.exe 用
VC1444_BIN="$WBO_WIN\\.lldbin\\vc1444\\Contents\\VC\\Tools\\MSVC\\14.44.35207\\bin\\Hostx64\\x64"

export RUSTFLAGS="-C linker=$WBO_WIN/.lldbin/lld-link.exe -C link-arg=/nodefaultlib:msvcrt -C link-arg=/nodefaultlib:OLDNAMES -C link-arg=libcmt.lib -C link-arg=libucrt.lib -C link-arg=libvcruntime.lib"
# stubs：legacy_stdio_definitions.lib 的空壳替代（rustc 固定传参；std 实际未引用其中符号）
export LIB="$VC_LIB;$WBO_WIN\\.lldbin\\crt1444\\Contents\\VC\\Tools\\MSVC\\14.44.35207\\lib\\x64;$WBO_WIN\\.lldbin\\stubs;$SDK\\Lib\\$SDK_VER\\ucrt\\x64;$SDK\\Lib\\$SDK_VER\\um\\x64"
# cc-rs 默认给 C 代码加 /MD（动态 CRT），与上面的静态 CRT 组合冲突（dllimport 对不上）——统一 /MT
export CFLAGS_x86_64_pc_windows_msvc=-MT
export CXXFLAGS_x86_64_pc_windows_msvc=-MT
export INCLUDE="$VC_INC;$SDK\\Include\\$SDK_VER\\ucrt;$SDK\\Include\\$SDK_VER\\um;$SDK\\Include\\$SDK_VER\\shared;$SDK\\Include\\$SDK_VER\\winrt;$SDK\\Include\\$SDK_VER\\cppwinrt"
# rc.exe（Tauri 打图标资源用）来自 SDK bin；cl.exe 来自现代工具集。
# 注意：Git Bash 的 PATH 必须 POSIX 风格（反斜杠路径会被冒号切坏），
# 而 RUSTFLAGS/LIB/INCLUDE 传给 Windows 工具链，保持反斜杠。
export PATH="$WBO_ROOT/.lldbin:$(cygpath -p "$VC1444_BIN"):$(cygpath -p "$SDK/bin/$SDK_VER/x64"):$PATH"

echo "[msvc-env] 便携链接环境已加载（lld-link + 静态 CRT）"
