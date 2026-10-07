# Релизная сборка MH Files (PLAN.md §11):
#   target\dist\MH-Files-<версия>-setup.exe     — установщик (если есть Inno Setup);
#   target\dist\MH-Files-<версия>-portable.zip  — переносная версия: exe и пустой файл
#                                                 `portable` — настройки хранятся рядом;
#   target\dist\MH-Files.exe и PDB              — для отладки сбоев.
#
# Подпись кода — если задан сертификат: `MH_SIGN_PFX` (путь к .pfx) и `MH_SIGN_PASSWORD`,
# либо `MH_SIGN_THUMBPRINT` (сертификат из хранилища пользователя, в том числе на токене).
# Подписываются exe, установщик и его деинсталлятор; метка времени — `MH_SIGN_TIMESTAMP`
# (по умолчанию DigiCert). Без сертификата всё собирается неподписанным, как раньше.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

# signtool из Windows SDK: самая новая версия x64.
function Find-SignTool {
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (-not (Test-Path $kits)) { return $null }
    Get-ChildItem $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' } |
        Sort-Object { $_.Directory.Parent.Name } -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

# Аргументы signtool без имени файла; $null — подписывать нечем.
function Get-SignArguments {
    $timestamp = if ($env:MH_SIGN_TIMESTAMP) { $env:MH_SIGN_TIMESTAMP } else { 'http://timestamp.digicert.com' }
    $common = @('sign', '/fd', 'sha256', '/tr', $timestamp, '/td', 'sha256', '/d', 'MH Files')
    if ($env:MH_SIGN_PFX) {
        if (-not (Test-Path $env:MH_SIGN_PFX)) { throw "нет файла сертификата $env:MH_SIGN_PFX" }
        return $common + @('/f', $env:MH_SIGN_PFX, '/p', $env:MH_SIGN_PASSWORD)
    }
    if ($env:MH_SIGN_THUMBPRINT) { return $common + @('/sha1', $env:MH_SIGN_THUMBPRINT) }
    return $null
}

$signArgs = Get-SignArguments
$signtool = $null
if ($signArgs) {
    $signtool = Find-SignTool
    if (-not $signtool) { throw "сертификат задан, но signtool (Windows SDK) не найден" }
}

function Invoke-Sign([string]$file) {
    if (-not $signArgs) { return }
    & $signtool @signArgs $file
    if ($LASTEXITCODE -ne 0) { throw "не подписан $file" }
}

cargo build --release --locked -p mh-files
if ($LASTEXITCODE -ne 0) { throw "сборка не удалась" }

$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq 'mh-files' } | Select-Object -ExpandProperty version
$dist = 'target\dist'
Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$dist\app" | Out-Null
Copy-Item 'target\release\MH-Files.exe' $dist -Force
Invoke-Sign "$dist\MH-Files.exe"
Copy-Item "$dist\MH-Files.exe" "$dist\app" -Force
if (Test-Path 'target\release\MH_Files.pdb') { Copy-Item 'target\release\MH_Files.pdb' $dist -Force }

# Переносная версия.
$portable = "$dist\portable\MH Files"
New-Item -ItemType Directory -Force -Path $portable | Out-Null
Copy-Item "$dist\MH-Files.exe" $portable
Copy-Item 'LICENSE' "$portable\LICENSE.txt"
Copy-Item 'assets\fonts\OFL.txt' "$portable\OFL-Cuprum.txt"
New-Item -ItemType File -Force -Path "$portable\portable" | Out-Null
Compress-Archive -Path $portable -DestinationPath "$dist\MH-Files-$version-portable.zip" -Force
Remove-Item -Recurse -Force "$dist\portable"

# Установщик: Inno Setup 6 (на GitHub Actions ставится шагом выше).
$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($iscc) {
    $numeric = ($version -split '[-+]')[0]
    $isccArgs = @("/DAppVersion=$version", "/DFileVersion=$numeric", "/DSourceDir=..\$dist\app")
    if ($signArgs) {
        # Inno Setup сам подписывает деинсталлятор и установщик командой `mhsign`:
        # $f — подписываемый файл, $q — кавычка.
        $quoted = ($signArgs | ForEach-Object { if ($_ -match '\s') { "`$q$_`$q" } else { $_ } }) -join ' '
        $isccArgs += @('/DSign', "/Smhsign=`$q$signtool`$q $quoted `$f")
    }
    & $iscc @isccArgs 'installer\MH-Files.iss'
    if ($LASTEXITCODE -ne 0) { throw "установщик не собрался" }
} else {
    Write-Warning "Inno Setup не найден — установщик не собран (переносная версия готова)"
}
Remove-Item -Recurse -Force "$dist\app"
if ($signArgs) { Write-Host "подписано: exe, установщик" } else { Write-Warning "сертификата нет — сборка не подписана (SmartScreen предупредит при первом запуске)" }
Get-ChildItem $dist | Format-Table Name, Length
