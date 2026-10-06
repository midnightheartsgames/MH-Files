# Релизная сборка MH Files: один exe и PDB рядом (PLAN.md §11).
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

cargo build --release --locked -p mh-files
if ($LASTEXITCODE -ne 0) { throw "сборка не удалась" }

$dist = 'target\dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Copy-Item 'target\release\MH-Files.exe' $dist -Force
if (Test-Path 'target\release\MH_Files.pdb') { Copy-Item 'target\release\MH_Files.pdb' $dist -Force }
Get-ChildItem $dist | Format-Table Name, Length
