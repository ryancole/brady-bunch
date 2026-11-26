<#
.SYNOPSIS
Installs or updates brady-bunch from its GitHub releases.

.DESCRIPTION
Downloads the release zip, checks it against the release's .sha256 file, and copies
brady-bunch.exe, brady-ctl.exe, brady-shim.exe and the docs into one folder
(default: %LOCALAPPDATA%\Programs\brady-bunch), replacing an older version in place.
That folder is added to your user PATH, so brady-ctl works from any shell.

Run it again to update. It refuses while brady-bunch is running from that folder.

    irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1 | iex

With options (a pinned version, a check for updates):

    & ([scriptblock]::Create((irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1))) -Version 0.6.0
    & ([scriptblock]::Create((irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1))) -Check

Whatever a config's min_version needs (nothing, if the installed version already meets it):

    & ([scriptblock]::Create((irm https://github.com/ryancole/brady-bunch/releases/latest/download/install.ps1))) -Config C:\src\app\brady.toml

.PARAMETER Version
The version to install, e.g. 0.6.0. Default: the latest release.

.PARAMETER Config
A brady-bunch config. Installs what it needs: nothing if the installed version meets
its min_version (or it has none and brady-bunch is installed), else the latest release
(or -Version), which must meet min_version. For wrappers that run brady-ctl up.

.PARAMETER InstallDir
Where to install. Default: %LOCALAPPDATA%\Programs\brady-bunch.

.PARAMETER Check
Only print the installed and the latest (or requested) version.

.PARAMETER NoPath
Don't add the install folder to the user PATH.

.PARAMETER Force
Reinstall even if that version is already installed.
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$Config,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\brady-bunch'),
    [switch]$Check,
    [switch]$NoPath,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is far slower with the progress bar.
$repo = 'ryancole/brady-bunch'
# Windows PowerShell 5.1 may not offer TLS 1.2 by default.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$InstallDir = [IO.Path]::GetFullPath($InstallDir)

function Get-InstalledVersion {
    $ctl = Join-Path $InstallDir 'brady-ctl.exe'
    if (-not (Test-Path $ctl)) { return $null }
    $out = & $ctl --version 2>$null
    if ("$out" -match 'brady-bunch (\d+\.\d+\.\d+)') { return $Matches[1] }
    return 'unknown (older than 0.6.0)'
}

function Add-ToUserPath {
    if ($NoPath) { return }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ })
    $already = $parts | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') }
    if (-not $already) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $InstallDir) -join ';'), 'User')
        Write-Host "Added $InstallDir to your user PATH (new shells will see it)."
    }
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
        $env:Path = "$env:Path;$InstallDir"
    }
}

function ConvertTo-VersionNumber([string]$v) {
    if ($v -notmatch '^\s*v?(\d+)(?:\.(\d+))?(?:\.(\d+))?\s*$') { return $null }
    [version]::new([int]$Matches[1], [int]("0$($Matches[2])"), [int]("0$($Matches[3])"))
}

# The top-level min_version of a TOML config: before its first [table] header.
function Get-MinVersion([string]$path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Config $path not found." }
    foreach ($line in Get-Content -LiteralPath $path) {
        if ($line -match '^\s*\[') { break }
        if ($line -match '^\s*min_version\s*=\s*(?:"([^"]*)"|''([^'']*)'')') {
            $v = "$($Matches[1])$($Matches[2])"
            if (-not (ConvertTo-VersionNumber $v)) { throw "min_version in $path isn't a version like ""0.6.0"": $v" }
            return $v
        }
    }
    return $null
}

if ($Config) {
    $min = Get-MinVersion $Config
    $installedNow = Get-InstalledVersion
    $have = if ($installedNow) { ConvertTo-VersionNumber $installedNow }
    if ($have -and (-not $min -or $have -ge (ConvertTo-VersionNumber $min)) -and -not $Force) {
        Write-Output "brady-bunch $installedNow is installed in $InstallDir$(if ($min) { "; $Config needs $min or newer" })."
        Add-ToUserPath
        return
    }
    if ($Version -and $min -and (ConvertTo-VersionNumber $Version) -lt (ConvertTo-VersionNumber $min)) {
        throw "$Config needs brady-bunch $min or newer; -Version $Version is older."
    }
}

$headers = @{ 'User-Agent' = 'brady-bunch-install'; 'Accept' = 'application/vnd.github+json' }
$api = if ($Version) {
    "https://api.github.com/repos/$repo/releases/tags/v$($Version.TrimStart('v'))"
} else {
    "https://api.github.com/repos/$repo/releases/latest"
}
$release = Invoke-RestMethod $api -Headers $headers
$target = $release.tag_name.TrimStart('v')
$installed = Get-InstalledVersion

if ($Check) {
    Write-Output "installed: $(if ($installed) { $installed } else { 'none' }) ($InstallDir)"
    Write-Output "$(if ($Version) { 'requested' } else { 'latest' }):    $target"
    # `return`, not `exit`: run through iex, exit would close the caller's shell.
    return
}

if ($Config -and $min -and (ConvertTo-VersionNumber $target) -lt (ConvertTo-VersionNumber $min)) {
    throw "$Config needs brady-bunch $min or newer, but the latest release is $target."
}

if ($installed -eq $target -and -not $Force) {
    Write-Output "brady-bunch $target is already installed in $InstallDir."
    Add-ToUserPath
    return
}

$running = Get-Process brady-bunch, brady-ctl, brady-shim -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir + '\', [StringComparison]::OrdinalIgnoreCase) }
if ($running) {
    $pids = ($running | ForEach-Object Id) -join ', '
    throw "brady-bunch is running from $InstallDir (pid $pids). Stop it first (brady-ctl down <config>), then run this again."
}

$zipAsset = $release.assets | Where-Object name -like '*-x86_64-windows.zip' | Select-Object -First 1
$shaAsset = $release.assets | Where-Object name -eq "$($zipAsset.name).sha256" | Select-Object -First 1
if (-not $zipAsset -or -not $shaAsset) {
    throw "Release $($release.tag_name) has no Windows zip with a .sha256 file."
}

$temp = Join-Path ([IO.Path]::GetTempPath()) "brady-bunch-install-$([guid]::NewGuid())"
New-Item -ItemType Directory $temp | Out-Null
try {
    $zip = Join-Path $temp $zipAsset.name
    $sha = Join-Path $temp $shaAsset.name
    Invoke-WebRequest $zipAsset.browser_download_url -OutFile $zip -UseBasicParsing -Headers $headers
    Invoke-WebRequest $shaAsset.browser_download_url -OutFile $sha -UseBasicParsing -Headers $headers
    $expected = ((Get-Content $sha -Raw).Trim() -split '\s+')[0]
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash
    if ($actual -ine $expected) {
        throw "SHA-256 mismatch for $($zipAsset.name): expected $expected, got $actual. Nothing was installed."
    }
    $extract = Join-Path $temp 'files'
    Expand-Archive $zip $extract
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Copy-Item (Join-Path $extract '*') $InstallDir -Recurse -Force
} finally {
    Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
}

Add-ToUserPath
$was = if ($installed) { " (was $installed)" } else { '' }
Write-Output "Installed brady-bunch $target$was in $InstallDir."
