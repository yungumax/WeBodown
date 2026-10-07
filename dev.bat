@echo off
chcp 65001 >nul
cd /d "%~dp0"

rem 开发环境自带的 Node 不在系统 PATH 里，这里显式加上
set "NODE_DIR=C:\Users\87845\.workbuddy\binaries\node\versions\22.22.2-3"
if not exist "%NODE_DIR%\npm.cmd" (
    echo [ERROR] 未找到 Node: %NODE_DIR%
    echo 请修改本脚本里的 NODE_DIR 指向实际安装目录。
    pause
    exit /b 1
)

rem 便携 MSVC 链接环境（本机未装 VS Build Tools）：
rem rust-lld 链接器 + VC14 静态 CRT + Windows SDK 库
set "WBO=%~dp0"
set "SDK=C:\Program Files (x86)\Windows Kits\10"
set "SDKVER=10.0.26100.0"
set "VC_LIB=%WBO%.lldbin\x64_extract\Program Files\Microsoft Visual Studio 14.0\VC\lib\amd64"
set "VC_INC=%WBO%.lldbin\headers_extract\Program Files\Microsoft Visual Studio 14.0\VC\include"
set "VC1444_BIN=%WBO%.lldbin\vc1444\Contents\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64"
rem 更新包签名密钥：tauri build 只读 TAURI_SIGNING_PRIVATE_KEY（值可为路径），
rem TAURI_SIGNING_PRIVATE_KEY_PATH 仅 signer sign 用；两者都设，缺一会出未签名包
set "TAURI_SIGNING_PRIVATE_KEY=D:\Zcode\_data\tauri-keys\bilidown.key"
set "TAURI_SIGNING_PRIVATE_KEY_PATH=D:\Zcode\_data\tauri-keys\bilidown.key"
set "TAURI_SIGNING_PRIVATE_KEY_PASSWORD="
set "RUSTFLAGS=-C linker=%WBO%.lldbin\lld-link.exe -C link-arg=/nodefaultlib:msvcrt -C link-arg=/nodefaultlib:OLDNAMES -C link-arg=libcmt.lib -C link-arg=libucrt.lib -C link-arg=libvcruntime.lib"
set "LIB=%VC_LIB%;%WBO%.lldbin\crt1444\Contents\VC\Tools\MSVC\14.44.35207\lib\x64;%WBO%.lldbin\stubs;%SDK%\Lib\%SDKVER%\ucrt\x64;%SDK%\Lib\%SDKVER%\um\x64"
set "CFLAGS_x86_64_pc_windows_msvc=-MT"
set "CXXFLAGS_x86_64_pc_windows_msvc=-MT"
set "INCLUDE=%VC_INC%;%SDK%\Include\%SDKVER%\ucrt;%SDK%\Include\%SDKVER%\um;%SDK%\Include\%SDKVER%\shared;%SDK%\Include\%SDKVER%\winrt;%SDK%\Include\%SDKVER%\cppwinrt"
set "PATH=%WBO%.lldbin;%VC1444_BIN%;%SDK%\bin\%SDKVER%\x64;%NODE_DIR%;%PATH%"

if /i "%~1"=="build" (
    echo 正在打包安装程序 ...
    call npm run tauri build
) else (
    echo 正在启动开发模式（前端热更新 + 桌面窗口）...
    call npm run tauri dev
)

echo.
echo 命令已结束。
pause >nul
