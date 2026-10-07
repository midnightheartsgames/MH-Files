# Релизная сборка MH Files (PLAN.md §11):
#   target\dist\MH-Files-<версия>-setup.exe     — установщик (если есть Inno Setup);
#   target\dist\MH-Files-<версия>-portable.zip  — переносная версия: exe и пустой файл
#                                                 `portable` — настройки хранятся рядом;
#   target\dist\MH-Files.exe и PDB              — для отладки сбоев.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

cargo build --release --locked -p mh-files
if ($LASTEXITCODE -ne 0) { throw "сборка не удалась" }

$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq 'mh-files' } | Select-Object -ExpandProperty version
$dist = 'target\dist'
Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$dist\app" | Out-Null
Copy-Item 'target\release\MH-Files.exe' $dist -Force
Copy-Item 'target\release\MH-Files.exe' "$dist\app" -Force
if (Test-Path 'target\release\MH_Files.pdb') { Copy-Item 'target\release\MH_Files.pdb' $dist -Force }

# Переносная версия.
$portable = "$dist\portable\MH Files"
New-Item -ItemType Directory -Force -Path $portable | Out-Null
Copy-Item 'target\release\MH-Files.exe' $portable
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
    & $iscc "/DAppVersion=$version" "/DFileVersion=$numeric" "/DSourceDir=..\$dist\app" 'installer\MH-Files.iss'
    if ($LASTEXITCODE -ne 0) { throw "установщик не собрался" }
} else {
    Write-Warning "Inno Setup не найден — установщик не собран (переносная версия готова)"
}
Remove-Item -Recurse -Force "$dist\app"
Get-ChildItem $dist | Format-Table Name, Length
