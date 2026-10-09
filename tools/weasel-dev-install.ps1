# Weasel DEV 版安装/卸载/启动：dev 身份二进制（output-dev\）注册为独立输入法
# 「衡Dev」，与正式版（output\）完全并行：
#   - CLSID/Profile 等 GUID 不同 → 输入法列表两项共存
#   - IPC 管道名不同 → 两套服务进程并存互不误杀
#   - 数据隔离（编译期 HENG_DEV 决定，不靠环境变量）：
#       共享数据 = exe 同级 data\      （WeaselSharedDataPath）
#       用户数据 = exe 同级 runtime-user\（WeaselUserDataPath dev 分支）
# 对标 tools/squirrel-dev-install.sh（macOS 测试版隔离部署）。
#
# 用法（注册需要管理员 PowerShell）：
#   .\tools\weasel-dev-install.ps1            # 注册 dev 输入法 + 启动 dev 服务
#   .\tools\weasel-dev-install.ps1 -StartOnly # 跳过注册，只启动 dev 服务
#   .\tools\weasel-dev-install.ps1 -Uninstall # 反注册 + 停 dev 服务（正式版不动）
# 注意：手动重启 dev 服务请始终用本脚本（-StartOnly），不要直接双击
# output-dev\WeaselServer.exe——双击时数据目录检查/初始化不会执行。
param([switch]$StartOnly, [switch]$Uninstall)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$outDev = Join-Path $root "third_party\src\weasel\output-dev"
$dll = Join-Path $outDev "weaselx64.dll"
$exe = Join-Path $outDev "WeaselServer.exe"

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not (Test-Path $exe)) {
  Write-Host "[FAIL] 未找到 $exe —— 先跑 build-x64-dev.bat"; exit 1
}

if ($Uninstall) {
  if (-not $isAdmin) { Write-Host "[FAIL] 反注册需要管理员 PowerShell"; exit 1 }
  regsvr32 /u /s $dll
  Write-Host "[OK] dev 输入法已反注册（dev 服务进程请自行关闭，正式版不受影响）"
  exit 0
}

if (-not $StartOnly) {
  if (-not $isAdmin) {
    Write-Host "[FAIL] 注册需要管理员 PowerShell（右键 -> 以管理员身份运行）"; exit 1
  }
  if (-not (Test-Path $dll)) {
    Write-Host "[FAIL] 未找到 $dll —— 先跑 build-x64-dev.bat"; exit 1
  }
}

# dev 二进制的运行依赖：从正式版 output\ 拷贝缺失项。
# WeaselServer.exe 导入 heng_core.dll / rime.dll / WinSparkle.dll，缺一则启动闪退；
# weaselx64.dll 仅依赖系统库。必须在启动/注册之前就位。
foreach ($dep in @("heng_core.dll", "rime.dll", "WinSparkle.dll")) {
  $src = Join-Path $root "third_party\src\weasel\output\$dep"
  $dst = Join-Path $outDev $dep
  if (-not (Test-Path $dst)) {
    if (-not (Test-Path $src)) {
      Write-Host "[FAIL] 缺少 $src —— 先跑 build-x64.bat 产出正式版"; exit 1
    }
    Copy-Item $src $dst -Force
    Write-Host "[OK] 依赖拷贝 $dep -> output-dev\"
  }
}

# dev 共享数据 = exe 同级 data\（WeaselSharedDataPath），首次从正式版 output\data
# 整目录拷贝（雾凇方案/词库快照，约 50MB）；用户数据 runtime-user\ 由引擎自动创建。
$srcData = Join-Path $root "third_party\src\weasel\output\data"
$dstData = Join-Path $outDev "data"
if ((Test-Path $srcData) -and -not (Test-Path $dstData)) {
  Copy-Item $srcData $dstData -Recurse -Force
  Write-Host "[OK] 雾凇共享数据初始化 -> output-dev\data（约 50MB，请稍候）"
}

if (-not $StartOnly) {
  $regsvr = Join-Path $env:SystemRoot "System32\regsvr32.exe"
  & $regsvr /s $dll | Out-Null
  $rc = $LASTEXITCODE
  if ($rc -ne 0) {
    Write-Host "[FAIL] regsvr32 /s 退出码 $rc —— 现在弹出对话框显示具体原因（请把内容发我）："
    & $regsvr $dll | Out-Null
    exit 1
  }
  Write-Host "[OK] dev 输入法已注册（列表名：衡Dev）"
}

# 启动 dev 服务。首次启动 core 会做一次完整部署（几十秒），期间打字暂时透传英文，
# 部署完成后自动可用。
$devProc = Start-Process $exe -PassThru
$alive = $false
for ($i = 0; $i -lt 10; $i++) {
  Start-Sleep -Milliseconds 500
  $devProc.Refresh()
  if ($devProc.HasExited) { break }
  $alive = $true; break
}
if ($alive) {
  Write-Host "[OK] dev 服务已启动 PID=$($devProc.Id)"
  Write-Host "    切换输入法：Win+空格 选「衡Dev」即可测试，正式版随时可切回"
  Write-Host "    首次启动部署约需几十秒，期间打字是英文属正常"
} else {
  Write-Host "[FAIL] dev 服务启动失败（闪退，查 %TEMP%\rime.weasel 日志或依赖路径）"; exit 1
}
