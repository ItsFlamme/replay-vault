# Builds a release into dist\: the portable zip, the installer (when Inno Setup 6 is
# installed) and SHA256SUMS.txt. Run from the repo root: powershell -File tools\release.ps1
# Release builds never carry the "Dev build" label, whatever branch they come from.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($env:GITHUB_REF_TYPE -eq 'tag' -and $env:GITHUB_REF_NAME -ne "v$version") {
    throw "Tag $env:GITHUB_REF_NAME doesn't match Cargo.toml version $version"
}

$env:RV_RELEASE = '1'
cargo build --release --locked --target-dir target\dist
if ($LASTEXITCODE) { throw 'cargo build failed' }

$dist = Join-Path (Get-Location) 'dist'
$stage = Join-Path $dist 'stage'
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory $stage | Out-Null
Copy-Item target\dist\release\replay-vault.exe, LICENSE, NOTICE, THIRD-PARTY-NOTICES.md, README.md $stage

$exe = Join-Path $dist "ReplayVault-$version.exe"
Copy-Item (Join-Path $stage 'replay-vault.exe') $exe
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath (Join-Path $dist "ReplayVault-$version-portable.zip")

$iscc = @(
    (Get-Command iscc -ErrorAction SilentlyContinue).Source,
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if ($iscc) {
    & $iscc /Q "/DAppVersion=$version" "/DSrcDir=$stage" "/DOutDir=$dist" installer\replay-vault.iss
    if ($LASTEXITCODE) { throw 'Inno Setup failed' }
} else {
    Write-Warning 'Inno Setup 6 not found: no installer this time.'
}

Remove-Item $stage -Recurse -Force
if (cargo --list | Select-String -Quiet cyclonedx) {
    cargo cyclonedx --format json --override-filename sbom
    if ($LASTEXITCODE) { throw 'SBOM failed' }
    Move-Item sbom.json (Join-Path $dist "ReplayVault-$version-sbom.cdx.json")
} else {
    Write-Warning 'cargo-cyclonedx not installed: no SBOM this time.'
}
Get-ChildItem $dist -File | Where-Object Name -ne 'SHA256SUMS.txt' | Sort-Object Name | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
} | Out-File (Join-Path $dist 'SHA256SUMS.txt') -Encoding ascii
Get-Content (Join-Path $dist 'SHA256SUMS.txt')
