; 安装完成后的收尾动作（由 tauri bundle.windows.nsis.installerHooks 引入）。
;
; 背景：tauri 生成的 NSIS 脚本在「覆盖安装」（UpdateMode=1）时**不会重建桌面快捷方式**
; （见 CreateOrUpdateDesktopShortcut 里的提前 Return）。而 Windows 按快捷方式缓存图标，
; 于是换了应用图标重新打包后，用户覆盖安装，桌面图标仍是旧的那一张。
;
; 两步处理：
; 1. 通知 shell 重建图标缓存（不需要重启资源管理器）；
; 2. 若桌面已存在同名快捷方式，删掉重建一次，让它重新绑定新 exe 的图标。
;    只处理产品名对应的那一个文件，不动用户改名后的副本。
!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "刷新图标缓存…"
  nsExec::Exec '"$SYSDIR\ie4uinit.exe" -show'
  Pop $0

  ${If} $NoShortcutMode != 1
    ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      Delete "$DESKTOP\${PRODUCTNAME}.lnk"
      CreateShortCut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    ${EndIf}
  ${EndIf}
!macroend
