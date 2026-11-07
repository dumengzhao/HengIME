# 本文件必须保存为 UTF-8 **带 BOM**：Windows PowerShell 5.1 读取无 BOM 的 .ps1
# 时按本地代码页（GBK）解码，中文注释会直接导致语法错误。
#
# 安装到 Program Files：复制产物、注册 TSF、拉起服务。
#
# 为什么要装到 Program Files 而不是仓库目录：候选窗要靠 uiAccess 令牌进入
# ZBID_UIACCESS 带，才能压住任务栏搜索面板这类 immersive 窗口。而 uiAccess
# 令牌有四个硬条件，缺一条就退回桌面带：清单声明、签名链到机器级受信根、
# 位于普通用户不可写的目录、由 shell 启动。本脚本负责后两条（位置与启动），
# 签名由 uia-sign.ps1 负责，证书导入由 -CertPath 负责。
#
# 用法（管理员 PowerShell，全部幂等）：
#   .\tools\weasel-uia-install.ps1                 # 装 output\（正式产物）
#   .\tools\weasel-uia-install.ps1 -Dev            # 装 output-dev\，自动补清单与签名，
#                                                  # 并把 dev 词库搬进目标目录
#   .\tools\weasel-uia-install.ps1 -CoreOnly       # 只换 heng_core.dll 并重启服务
#   .\tools\weasel-uia-install.ps1 -CertPath .\heng-uia.cer   # 首次导证书
#   .\tools\weasel-uia-install.ps1 -Uninstall
param(
    [string]$Source,
    [string]$Target = 'C:\Program Files\HengIME',
    [string]$CertPath,
    [string]$Thumbprint,
    [switch]$SkipCert,
    [switch]$NoAutostart,
    [switch]$CoreOnly,
    [switch]$Dev,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Source) {
    $Source = Join-Path $root $(if ($Dev) { 'third_party\src\weasel\output-dev' } else { 'third_party\src\weasel\output' })
}
$signScript = Join-Path $root 'third_party\src\weasel\uia-sign.ps1'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host '[FAIL] 需要管理员 PowerShell（右键 -> 以管理员身份运行）'
    exit 1
}

$targetExe = Join-Path $Target 'WeaselServer.exe'
$targetDll = Join-Path $Target 'weaselx64.dll'
$targetCore = Join-Path $Target 'heng_core.dll'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

# 停掉所有 WeaselServer：安装会把 TSF 重注册到新目录，旧目录（如 output-dev）
# 里的服务若还在跑，命名管道与用户词库都握在它手上，新服务起不来或起成哑的。
# 必须等进程真正退出——Stop-Process 是异步的，紧接着复制会撞上「文件被占用」。
function Stop-ServerProcess {
    $procs = @(Get-Process -Name WeaselServer,WeaselDeployer -ErrorAction SilentlyContinue)
    foreach ($p in $procs) {
        $path = '?'
        try { $path = $p.Path } catch { }
        Write-Host "[i] 停止 $path PID=$($p.Id)"
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
    if ($procs.Count -gt 0) {
        $procs | Wait-Process -Timeout 8 -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 200
    }
}

# 目标文件被映射成镜像时无法覆盖、但可以改名：把占用者挪成 .old<时间戳>，
# 新文件就能落地；残留等下次运行清（能删的删，仍被映射的留着）。
function Clear-LockedTarget([string]$path) {
    if (-not (Test-Path $path)) { return }
    try {
        $fs = [System.IO.File]::Open($path, 'Open', 'ReadWrite', 'None')
        $fs.Close()
        return
    } catch { }
    $suffix = Get-Date -Format 'HHmmss'
    $parked = "$path.old$suffix"
    try {
        Move-Item -LiteralPath $path -Destination $parked -Force
        Write-Host "[i] 已把占用中的 $(Split-Path $path -Leaf) 挪到 $(Split-Path $parked -Leaf)"
    } catch {
        Write-Host "[FAIL] $(Split-Path $path -Leaf) 既不能写也不能改名：$($_.Exception.Message)"
        Write-Host '       通常是还有进程在用它，或者当前 PowerShell 并非管理员。'
        exit 1
    }
}

function Sweep-Parked([string]$dir) {
    Get-ChildItem $dir -Filter '*.old*' -ErrorAction SilentlyContinue | ForEach-Object {
        Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
    }
}

if ($Uninstall) {
    Stop-ServerProcess
    if (Test-Path $targetDll) {
        & (Join-Path $env:SystemRoot 'System32\regsvr32.exe') /u /s $targetDll | Out-Null
        Write-Host '[OK] 已反注册 TSF'
    }
    Remove-ItemProperty -Path $runKey -Name 'HengServer' -ErrorAction SilentlyContinue
    Write-Host "[OK] 已移除登录自启（目录 $Target 保留，需手动删）"
    exit 0
}

# 只换 core dll：改定位/渲染逻辑后用它，省掉 50MB 共享数据的复制
if ($CoreOnly) {
    $srcCore = Join-Path $Source 'heng_core.dll'
    if (-not (Test-Path $srcCore)) { Write-Host "[FAIL] 找不到 $srcCore"; exit 1 }
    if (-not (Test-Path $Target)) { Write-Host "[FAIL] $Target 未安装，先跑不带 -CoreOnly 的安装"; exit 1 }
    Stop-ServerProcess
    Sweep-Parked $Target
    Clear-LockedTarget $targetCore
    Copy-Item $srcCore $targetCore -Force
    Write-Host "[OK] 已更新 $targetCore"
    Start-Process explorer.exe -ArgumentList "`"$targetExe`""
    Start-Sleep -Seconds 2
    $p = Get-Process -Name WeaselServer -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($Target, 'OrdinalIgnoreCase') } |
        Select-Object -First 1
    if ($p) { Write-Host "[OK] 服务已重启 PID=$($p.Id)" } else { Write-Host '[WARN] 服务没起来，切到任意输入框打一个字会触发 TSF 拉起' }
    exit 0
}

# 0) dev 产物没有签名：build-x64-dev.bat 只编 msbuild，不跑 uia-sign.ps1。
#    装到 Program Files 前必须补上清单与签名，否则拿不到 uiAccess 令牌。
if ($Dev) {
    $devExe = Join-Path $Source 'WeaselServer.exe'
    if (-not (Test-Path $devExe)) {
        Write-Host "[FAIL] 找不到 $devExe —— 先跑 build-x64-dev.bat"; exit 1
    }
    if (-not $Thumbprint) {
        # 先沿用当前已装 exe 的签名者，再退回证书库里带私钥的 HengIME 证书
        if (Test-Path $targetExe) {
            $cur = Get-AuthenticodeSignature -FilePath $targetExe
            if ($cur.SignerCertificate) { $Thumbprint = $cur.SignerCertificate.Thumbprint }
        }
        if (-not $Thumbprint) {
            $c = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
                Where-Object { $_.Subject -like '*HengIME*' -and $_.HasPrivateKey } |
                Sort-Object NotAfter -Descending | Select-Object -First 1
            if ($c) { $Thumbprint = $c.Thumbprint }
        }
        if (-not $Thumbprint) { Write-Host '[FAIL] 找不到签名证书，用 -Thumbprint <指纹> 指定'; exit 1 }
    }
    Write-Host "[i] 给 dev 产物补 uiAccess 清单与签名（$Thumbprint）…"
    try {
        & $signScript -Exe $devExe -Thumbprint $Thumbprint | ForEach-Object { Write-Host "    $_" }
    } catch {
        Write-Host "[FAIL] dev 产物签名失败：$($_.Exception.Message)"; exit 1
    }
}

# 1) 产物自检：没有 uiAccess 清单或签名无效，装了也不会有任何效果
if (-not (Test-Path $targetExe) -and -not (Test-Path (Join-Path $Source 'WeaselServer.exe'))) {
    Write-Host "[FAIL] 未找到 $(Join-Path $Source 'WeaselServer.exe') —— 先跑 build-x64.bat"; exit 1
}
$srcExe = Join-Path $Source 'WeaselServer.exe'
if (Test-Path $srcExe) {
    Write-Host '[i] 校验产物清单与签名…'
    try {
        & $signScript -Exe $srcExe -Verify | ForEach-Object { Write-Host "    $_" }
    } catch {
        Write-Host '[FAIL] 产物没有 uiAccess 清单或签名无效。'
        Write-Host '       dev 产物用 -Dev 让它自动补签名；正式产物检查 HENG_SIGN_THUMBPRINT。'
        exit 1
    }
}

# 2) 证书：uiAccess 校验在 SYSTEM 上下文做，必须进机器级受信根。
#    若产物的签名者已在 LocalMachine\Root（上一次装过），无需再给 -CertPath。
if (-not $SkipCert) {
    $needCert = $true
    if (Test-Path $srcExe) {
        $sgn = Get-AuthenticodeSignature -FilePath $srcExe
        if ($sgn.SignerCertificate) {
            $tp = $sgn.SignerCertificate.Thumbprint
            $trusted = Get-ChildItem Cert:\LocalMachine\Root -ErrorAction SilentlyContinue |
                Where-Object { $_.Thumbprint -eq $tp }
            if ($trusted) {
                Write-Host "[i] 签名证书已在 LocalMachine\Root（$tp），跳过导入"
                $needCert = $false
            } else {
                Write-Host "[i] 签名证书 $tp 不在 LocalMachine\Root"
            }
        }
    }
    if ($needCert) {
        if (-not $CertPath) {
            Write-Host '[FAIL] 签名证书未进机器级受信根，需 -CertPath <cer>'
            Write-Host '       证书可从 Cert:\CurrentUser\My 里导出：'
            Write-Host '       Export-Certificate -Cert (Get-ChildItem Cert:\CurrentUser\My\<指纹>) -FilePath .\heng-uia.cer'
            exit 1
        }
        if (-not (Test-Path $CertPath)) { Write-Host "[FAIL] 找不到证书 $CertPath"; exit 1 }
        $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2(
            (Resolve-Path $CertPath).Path)
        foreach ($n in @('Root', 'TrustedPublisher')) {
            $st = New-Object System.Security.Cryptography.X509Certificates.X509Store($n, 'LocalMachine')
            $st.Open('ReadWrite')
            if (-not ($st.Certificates | Where-Object { $_.Thumbprint -eq $cert.Thumbprint })) {
                $st.Add($cert)
                Write-Host "[OK] 证书已导入 LocalMachine\$n"
            } else {
                Write-Host "[i] LocalMachine\$n 已有该证书"
            }
            $st.Close()
        }
    }
}

# 3) 目标位置：必须在普通用户不可写的目录，否则 uiAccess 令牌不会下发
if (-not $Target.StartsWith('C:\Program Files', 'OrdinalIgnoreCase')) {
    Write-Host "[FAIL] 目标目录必须在 Program Files 下（当前 $Target）"; exit 1
}
Stop-ServerProcess
if (-not (Test-Path $Target)) { New-Item -ItemType Directory -Path $Target | Out-Null }
# 覆盖前反注册目标目录里的旧 dll。源可能换了身份（正式版与 dev 的 CLSID / Profile
# 是两套），不反注册会在注册表里留下指向同一路径、内容却已被替换掉的旧 CLSID，
# 输入法列表里就多出一项点不动的项。
$regsvr = Join-Path $env:SystemRoot 'System32\regsvr32.exe'
if (Test-Path $targetDll) {
    & $regsvr /u /s $targetDll | Out-Null
    Write-Host '[i] 已反注册目标目录里的旧 dll'
}
# 覆盖前先解占用：服务已停，但 weaselx64.dll 还被接过输入法的宿主进程映射着
# （记事本、浏览器……），直接 robocopy 会撞「另一个程序正在使用此文件」。
Sweep-Parked $Target
foreach ($f in @($targetCore, $targetDll, $targetExe)) { Clear-LockedTarget $f }
$rc = (Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\robocopy.exe') -Wait -PassThru -NoNewWindow `
        -ArgumentList @("`"$Source`"", "`"$Target`"", '/E', '/COPY:DAT', '/R:1', '/W:1', '/NFL', '/NDL', '/NJH', '/NJS')).ExitCode
if ($rc -gt 7) { Write-Host "[FAIL] robocopy 退出码 $rc"; exit 1 }
Write-Host "[OK] 已同步到 $Target"

# 3b) dev 版的用户数据在 exe 同级 runtime-user\（WeaselUtility.cpp 的 HENG_DEV 分支），
#     搬进 Program Files 后它会在新位置找一个空的，原有词库不会跟过来。目标为空时
#     把源目录整体复制过去；目标已有内容则不动，避免覆盖正在用的词库。
if ($Dev) {
    $srcUser = Join-Path $Source 'runtime-user'
    $dstUser = Join-Path $Target 'runtime-user'
    if (Test-Path $srcUser) {
        if (Test-Path (Join-Path $dstUser 'build')) {
            Write-Host '[i] 目标已有 runtime-user，跳过词库搬迁'
        } else {
            Write-Host '[i] 搬迁 dev 词库（runtime-user）…'
            $rc2 = (Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\robocopy.exe') -Wait -PassThru -NoNewWindow `
                    -ArgumentList @("`"$srcUser`"", "`"$dstUser`"", '/E', '/COPY:DAT', '/R:1', '/W:1', '/NFL', '/NDL', '/NJH', '/NJS')).ExitCode
            if ($rc2 -gt 7) { Write-Host "[FAIL] 词库搬迁失败，robocopy 退出码 $rc2"; exit 1 }
            Write-Host "[OK] 词库已复制到 $dstUser（源目录保留，确认无误后可自行删除）"
        }
    }
}

# 4) AppContainer（UWP）宿主读 DLL：Program Files 本身有继承 ACE，这里显式补齐，
#    避免目标目录被手工重建后丢掉。SDDL 判定不受系统语言影响。
$acl = Get-Acl $Target
if ($acl.Sddl -notmatch ';;;AC\)') {
    $acSid = New-Object System.Security.Principal.SecurityIdentifier 'S-1-15-2-1'
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
        $acSid, 'ReadAndExecute', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
    Set-Acl -Path $Target -AclObject $acl
    Write-Host '[OK] 已授予 AppContainer 读权限'
}

# 5) 注册 TSF（InprocServer32 存的是绝对路径，移动目录后必须重注册）
& $regsvr /s $targetDll | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Host "[FAIL] regsvr32 退出码 $LASTEXITCODE（再跑一次不带 /s 看对话框具体内容）"
    & $regsvr $targetDll | Out-Null
    exit 1
}
Write-Host '[OK] TSF 已注册'

# 6) 登录自启：uiAccess 进程只能由 shell 或另一个 uiAccess 进程拉起，Run 键由
#    Explorer 执行，正好满足这个条件；同时省掉首次按键时等 shell 转发的延迟。
if (-not $NoAutostart) {
    Set-ItemProperty -Path $runKey -Name 'HengServer' -Value ("`"$targetExe`"")
    Write-Host '[OK] 已设置登录自启（HKCU Run\HengServer）'
}

# 7) 启动服务：必须经 explorer（shell 是受信启动方，令牌才落得下来）
Start-Process explorer.exe -ArgumentList "`"$targetExe`""
Start-Sleep -Seconds 3
$p = Get-Process -Name WeaselServer -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($Target, 'OrdinalIgnoreCase') } |
    Select-Object -First 1
if (-not $p) {
    Write-Host '[FAIL] 服务未起来。常见原因：签名链没进机器级受信根、产物未签名、目录不在 Program Files'
    exit 1
}
Write-Host "[OK] 服务已启动 PID=$($p.Id)"

# 8) 令牌校验：进程起来不等于拿到 uiAccess。TokenUIAccess=26 是唯一可靠的
#    判据——四个条件（清单/签名/位置/启动方）任一不满足都会静默退回桌面带。
function Test-UiAccessToken([int]$procId) {
    $cs = @'
using System;
using System.Runtime.InteropServices;
public static class TokChk {
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern bool OpenProcessToken(IntPtr h, uint access, out IntPtr tok);
    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern bool GetTokenInformation(IntPtr tok, int cls, out int info, int len, out int ret);
    [DllImport("kernel32.dll")]
    public static extern bool CloseHandle(IntPtr h);
    public static int Check(int pid) {
        IntPtr p = OpenProcess(0x1000, false, pid);
        if (p == IntPtr.Zero) return -1;
        IntPtr t;
        if (!OpenProcessToken(p, 0x0008, out t)) { CloseHandle(p); return -2; }
        int val, ret;
        bool ok = GetTokenInformation(t, 26, out val, 4, out ret);
        CloseHandle(t); CloseHandle(p);
        if (!ok) return -3;
        return val;
    }
}
'@
    Add-Type -TypeDefinition $cs -ErrorAction Stop
    return [TokChk]::Check($procId)
}
try {
    $uia = Test-UiAccessToken $p.Id
    if ($uia -eq 1) {
        Write-Host '[OK] 令牌已带 uiAccess（候选窗会进 ZBID_UIACCESS 带）'
    } else {
        Write-Host "[WARN] 令牌没有 uiAccess（Check 返回 $uia）——候选窗仍会留在桌面带，"
        Write-Host '       只能靠定位躲避浮层。逐个核对：'
        Write-Host '       1. WeaselServer.exe 清单 uiAccess="true"（uia-sign.ps1 -Verify）'
        Write-Host '       2. 签名有效且证书在 LocalMachine\Root（不是 CurrentUser\Root）'
        Write-Host "       3. exe 在 $Target（普通用户不可写）"
        Write-Host '       4. 由 explorer 启动（本脚本第 7 步）——手工双击 exe 不满足这一条'
    }
} catch {
    Write-Host "[WARN] 令牌校验未执行：$($_.Exception.Message)"
}
Write-Host '     下一步：任务栏搜索框里打几个字，候选窗应当贴着输入行出现（不再被面板盖住）'
Write-Host '     注意：已经运行的应用还持有旧 weaselx64.dll，它们会继续用旧组件拉起旧目录的'
Write-Host '           服务。要排除这种干扰，先注销重登（或重启 explorer 后再试）。'
