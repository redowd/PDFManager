<#
.SYNOPSIS
    一键构建 PDF Manager：前端 production build -> Rust release build -> 替换根目录 PDFManager.exe。

.DESCRIPTION
    - 自动安装 / 校验前端依赖（缺失或不完整时执行 npm ci / npm install）
    - 前端构建成功后触碰 routes.rs，确保 rust-embed 重新内嵌最新前端
    - 检测正在运行的 PDFManager：先关窗口，超时未退出再强制结束，然后替换 exe
    - 任一步失败立即退出（退出码 1），不会继续产出「看似成功」的旧包

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\build.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Frontend = Join-Path $Root 'frontend'
$Backend = Join-Path $Root 'rust-backend'
$TargetExe = Join-Path $Root 'PDFManager.exe'
$BuiltExe = Join-Path $Backend 'target\release\pdf-manager.exe'
$Started = Get-Date

function Write-Step([string]$Text) {
    Write-Host "`n==> $Text" -ForegroundColor Cyan
}

function Fail([string]$Text) {
    Write-Host "`n[构建失败] $Text" -ForegroundColor Red
    Write-Host "根目录 PDFManager.exe 未被替换，仍为上一版产物（如有）。" -ForegroundColor Yellow
    exit 1
}

# 运行原生命令：临时关闭 Stop，避免 npm/cargo 写到 stderr 的普通告警被当成终止错误
function Invoke-Native([string]$Desc, [string]$File, [string[]]$Arguments, [string]$WorkDir) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $code = 0
    try {
        Push-Location $WorkDir
        & $File @Arguments
        $code = $LASTEXITCODE
    }
    finally {
        Pop-Location
        $ErrorActionPreference = $previous
    }
    if ($code -ne 0) { Fail "$Desc 失败（退出码 $code）" }
}

Write-Host "PDF Manager 一键构建" -ForegroundColor Green
Write-Host "项目目录: $Root"

if (-not (Test-Path $Frontend)) { Fail "缺少前端目录：$Frontend" }
if (-not (Test-Path $Backend)) { Fail "缺少后端目录：$Backend" }

Write-Step '检查构建工具'
foreach ($tool in @('node', 'npm', 'cargo')) {
    $cmd = Get-Command $tool -ErrorAction SilentlyContinue
    if (-not $cmd) { Fail "未找到 $tool，请先安装并加入 PATH" }
    $ver = (& $tool --version 2>&1 | Select-Object -First 1)
    Write-Host ("  {0}: {1}" -f $tool, $ver)
}

Write-Step '安装 / 校验前端依赖'
$needInstall = -not (Test-Path (Join-Path $Frontend 'node_modules'))
if (-not $needInstall) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    Push-Location $Frontend
    & npm ls --depth=0 2>&1 | Out-Null
    $lsCode = $LASTEXITCODE
    Pop-Location
    $ErrorActionPreference = $previous
    if ($lsCode -ne 0) {
        Write-Host '  node_modules 依赖不完整，准备重新安装' -ForegroundColor Yellow
        $needInstall = $true
    }
}
if ($needInstall) {
    if (Test-Path (Join-Path $Frontend 'package-lock.json')) {
        Write-Host '  执行 npm ci ...'
        Invoke-Native '前端依赖安装' 'npm' @('ci') $Frontend
    } else {
        Write-Host '  执行 npm install ...'
        Invoke-Native '前端依赖安装' 'npm' @('install') $Frontend
    }
} else {
    Write-Host '  node_modules 已存在且依赖完整，跳过安装'
}

Write-Step '前端 production build'
Invoke-Native '前端构建（npm run build）' 'npm' @('run', 'build') $Frontend

$indexHtml = Join-Path $Frontend 'build\index.html'
if (-not (Test-Path $indexHtml)) { Fail "前端构建产物缺失：$indexHtml" }

Write-Step 'Rust release build'
# rust-embed 在编译期读取 frontend/build，需让后端 crate 重新编译才能内嵌新前端
$anchor = Join-Path $Backend 'src\routes.rs'
if (Test-Path $anchor) { (Get-Item $anchor).LastWriteTime = Get-Date }
Invoke-Native 'Rust release 构建（cargo build --release）' 'cargo' @('build', '--release') $Backend

if (-not (Test-Path $BuiltExe)) { Fail "未找到构建产物：$BuiltExe" }

Write-Step '替换根目录 PDFManager.exe'
$running = @(Get-Process -Name 'PDFManager', 'pdf-manager' -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    $names = ($running | ForEach-Object { "$($_.ProcessName)($($_.Id))" }) -join ', '
    Write-Host "  检测到正在运行：$names，先关闭窗口" -ForegroundColor Yellow
    $running | ForEach-Object { $_.CloseMainWindow() | Out-Null }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        if (@(Get-Process -Name 'PDFManager', 'pdf-manager' -ErrorAction SilentlyContinue).Count -eq 0) { break }
    }
    $left = @(Get-Process -Name 'PDFManager', 'pdf-manager' -ErrorAction SilentlyContinue)
    if ($left.Count -gt 0) {
        Write-Host '  窗口 10 秒内未关闭，强制结束进程' -ForegroundColor Yellow
        $left | Stop-Process -Force
        Start-Sleep -Seconds 2
    }
}

$copied = $false
for ($i = 1; $i -le 5; $i++) {
    try {
        Copy-Item $BuiltExe $TargetExe -Force
        $copied = $true
        break
    }
    catch {
        Write-Host ("  第 {0} 次复制失败：{1}" -f $i, $_.Exception.Message) -ForegroundColor Yellow
        Start-Sleep -Seconds 2
    }
}
if (-not $copied) { Fail "无法写入 $TargetExe（文件可能仍被占用）" }

$file = Get-Item $TargetExe
if ($file.Length -le 0) { Fail '复制后的 exe 大小为 0，构建产物异常' }
$hash = (Get-FileHash $TargetExe -Algorithm SHA256).Hash
$elapsed = [int]((Get-Date) - $Started).TotalSeconds

Write-Host "`n构建完成" -ForegroundColor Green
Write-Host ("  产物: {0}" -f $file.FullName)
Write-Host ("  大小: {0:N0} 字节" -f $file.Length)
Write-Host ("  SHA256: {0}" -f $hash)
Write-Host ("  耗时: {0} 秒" -f $elapsed)
Write-Host "  可直接运行 .\PDFManager.exe"
