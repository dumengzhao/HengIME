; 衡 HengIME Windows x64 一键安装器
; 复刻 tools/weasel-uia-install.ps1 的安装语义：
;   停服务 -> 反注册旧 dll -> 装入 Program Files -> 证书进机器级受信根 ->
;   AppContainer ACE -> 注册 TSF -> 登录自启 -> 经 explorer 拉起服务。
; 构建命令（仓库根）：
;   "C:\Program Files (x86)\NSIS\makensis.exe" tools\windows-installer\heng-ime.nsi

!include "MUI2.nsh"

Unicode true
ManifestDPIAware true

!define ROOT "..\.."
!define EXEDIR "${ROOT}\third_party\src\weasel\output"
!define ICON "${ROOT}\third_party\src\weasel\resource\weasel.ico"
!define PRODUCT "HengIME"
!define VERSION "0.1.0"

Name "衡 (HengIME)"
OutFile "${ROOT}\dist\heng-ime-setup-${VERSION}.exe"
InstallDir "$PROGRAMFILES64\HengIME"
InstallDirRegKey HKLM "Software\${PRODUCT}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchServer
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

Function LaunchServer
  ; 服务必须由 shell 拉起才能拿到 uiAccess 令牌，不能用 ExecShell 直接跑
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\WeaselServer.exe"'
FunctionEnd

Section "Install"
  ; 停掉旧服务（Stop-Process 语义）：命名管道与词库握在它手上，不杀装不进去
  nsExec::Exec 'taskkill /f /im WeaselServer.exe'
  nsExec::Exec 'taskkill /f /im WeaselDeployer.exe'
  Sleep 800

  ; 反注册目标目录里的旧 dll（覆盖安装时清理旧 CLSID）
  IfFileExists "$INSTDIR\weaselx64.dll" 0 +2
    nsExec::Exec '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\weaselx64.dll"'

  ; 证书：uiAccess 校验要求签名链到机器级受信根，-f 幂等覆盖
  File "/oname=$PLUGINSDIR\heng-uia.cer" "${ROOT}\dist\pkg\heng-uia.cer"
  nsExec::ExecToLog 'certutil -f -addstore Root "$PLUGINSDIR\heng-uia.cer"'
  Pop $0
  nsExec::ExecToLog 'certutil -f -addstore TrustedPublisher "$PLUGINSDIR\heng-uia.cer"'
  Pop $0
  Delete "$PLUGINSDIR\heng-uia.cer"

  ; 产物本体
  SetOutPath "$INSTDIR"
  File "${EXEDIR}\WeaselServer.exe"
  File "${EXEDIR}\WeaselDeployer.exe"
  File "${EXEDIR}\weaselx64.dll"
  File "${EXEDIR}\heng_core.dll"
  File "${EXEDIR}\rime.dll"
  File "${EXEDIR}\WinSparkle.dll"
  SetOutPath "$INSTDIR\data"
  File /r "${EXEDIR}\data\*.*"

  ; AppContainer（UWP）宿主读 DLL：Program Files 自带继承 ACE，显式补齐防手工重建丢掉
  nsExec::ExecToLog 'icacls "$INSTDIR" /grant *S-1-15-2-1:(OI)(CI)(RX)'
  Pop $0

  ; 注册 TSF 输入法
  nsExec::ExecToLog '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\weaselx64.dll"'
  Pop $0

  ; 登录自启 + 记录安装目录（供覆盖安装 / 卸载器定位）+ 控制面板卸载项
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "HengServer" "$INSTDIR\WeaselServer.exe"
  WriteRegStr HKLM "Software\${PRODUCT}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "DisplayName" "衡 (HengIME)"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "DisplayIcon" "$INSTDIR\WeaselServer.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "Publisher" "HengIME"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}" "NoRepair" 1

  ; 卸载器
  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /f /im WeaselServer.exe'
  nsExec::Exec 'taskkill /f /im WeaselDeployer.exe'
  Sleep 800
  nsExec::Exec '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\weaselx64.dll"'
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "HengServer"
  DeleteRegKey /ifempty HKLM "Software\${PRODUCT}"
  DeleteRegKey /ifempty HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
  RMDir /r "$INSTDIR"
SectionEnd
