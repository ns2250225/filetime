param()
$ErrorActionPreference = 'Stop'
$project = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$bundle = Join-Path $project 'target\bundle'
$package = Join-Path $bundle 'package'
$sdk = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64'
$vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) { throw '未找到 Visual Studio Build Tools' }
$vs = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
if (-not $vs) { throw '未找到 MSVC C++ 工具链' }
$vc = Join-Path $vs 'Common7\Tools\VsDevCmd.bat'
New-Item -ItemType Directory -Force -Path $bundle,$package,(Join-Path $package 'Assets') | Out-Null
$batch = Join-Path $bundle 'compile-shell.cmd'
@(
    '@echo off',
    "call `"$vc`" -arch=x64 -host_arch=x64 >nul",
    'cl /nologo /utf-8 /std:c++17 /EHsc /O2 /LD /I"C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um\winsqlite" ..\..\shell-extension\FileTimeShell.cpp /link /OUT:FileTimeShell.dll /EXPORT:DllGetClassObject /EXPORT:DllCanUnloadNow'
) | Set-Content -LiteralPath $batch -Encoding Ascii
Push-Location $bundle
try { & cmd.exe /d /c "`"$batch`"" } finally { Pop-Location }
if ($LASTEXITCODE -ne 0) { throw 'COM 扩展编译失败' }
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'AppxManifest.xml') -Destination (Join-Path $package 'AppxManifest.xml') -Force
Copy-Item -LiteralPath (Join-Path $project 'assets\Logo.png') -Destination (Join-Path $package 'Assets\Logo.png') -Force
Copy-Item -LiteralPath (Join-Path $project 'assets\SmallLogo.png') -Destination (Join-Path $package 'Assets\SmallLogo.png') -Force
$msix = Join-Path $bundle 'FileTime.msix'
& (Join-Path $sdk 'makeappx.exe') pack /o /d $package /nv /p $msix
if ($LASTEXITCODE -ne 0) { throw '身份包构建失败' }
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq 'CN=FileTime Local' -and $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date).AddDays(30) } | Sort-Object NotBefore -Descending | Select-Object -First 1
if (-not $cert) { $cert = New-SelfSignedCertificate -Subject 'CN=FileTime Local' -Type CodeSigningCert -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy NonExportable -KeyLength 2048 -HashAlgorithm SHA256 }
Export-Certificate -Cert $cert -FilePath (Join-Path $bundle 'FileTime.cer') -Force | Out-Null
& (Join-Path $sdk 'signtool.exe') sign /fd SHA256 /sha1 $cert.Thumbprint /s My $msix
if ($LASTEXITCODE -ne 0) { throw '身份包签名失败' }
Push-Location $project
try {
    cargo build --release --features bundled-shell
    if ($LASTEXITCODE -ne 0) { throw '主程序构建失败' }
} finally { Pop-Location }
Write-Host "单文件交付：$(Join-Path $project 'target\release\filetime.exe')"
