; WeBodown 自定义 NSIS 钩子（tauri.conf.json -> bundle.windows.nsis.installerHooks）
;
; 背景：Tauri 内置的「删除应用程序数据」复选框只清理
;   $APPDATA\<identifier>  与  $LOCALAPPDATA\<identifier>（即 com.webodown.app），
; 而本应用的数据实际存放在 %APPDATA%\webodown\：
;   settings.json（设置）、cookies.json（登录凭据）、qrcode_debug.log（日志）。
; 不处理的话，卸载时勾选「删除应用程序数据」后重新安装，登录状态与设置仍然残留。
;
; 行为：勾选复选框时，清掉应用数据文件；
;   下载目录（默认 webodown\downloads，属于用户文件）保留不删，
;   目录本体仅在已空时移除。
; 注：更新流程（passive 模式）下复选框状态恒为 0，因此自动更新不会误删数据。

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
    ; 卸载段此前已 SetShellVarContext current，$APPDATA 即当前用户
    Delete "$APPDATA\webodown\settings.json"
    Delete "$APPDATA\webodown\cookies.json"
    Delete "$APPDATA\webodown\*.log"
    ; 仅当没有其它文件（如下载目录）时移除目录本体
    RMDir "$APPDATA\webodown"
  ${EndIf}
!macroend
