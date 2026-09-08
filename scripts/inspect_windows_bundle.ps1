[CmdletBinding()]
param(
    [string]$WorkspaceRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$BundleDirectory = "target/release/bundle",
    [string]$ExecutablePath = "target/release/music-folder-desktop.exe",
    [string]$CliExecutablePath = "target/release/music-folder.exe",
    [switch]$ConfigOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Resolve-WorkspacePath {
    param([string]$Path)
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $WorkspaceRoot $Path))
}

function Find-WindowsSdkTool {
    param([string]$Name)
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits/10/bin"
    Assert-Condition (Test-Path -LiteralPath $sdkRoot -PathType Container) "Windows SDK bin directory is missing: $sdkRoot"
    $candidate = Get-ChildItem -LiteralPath $sdkRoot -Directory |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName "x64/$Name" } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($candidate)) "$Name was not found in the Windows SDK"
    return $candidate
}

function Get-CspDirectives {
    param([string]$Policy)
    $directives = @{}
    foreach ($segment in $Policy.Split(";", [StringSplitOptions]::RemoveEmptyEntries)) {
        $tokens = @($segment.Trim() -split "\s+" | Where-Object { $_.Length -gt 0 })
        if ($tokens.Count -eq 0) {
            continue
        }
        $name = $tokens[0].ToLowerInvariant()
        Assert-Condition (-not $directives.ContainsKey($name)) "production CSP contains duplicate directive: $name"
        $values = if ($tokens.Count -gt 1) { @($tokens[1..($tokens.Count - 1)]) } else { @() }
        $directives[$name] = $values
    }
    return ,$directives
}

function Assert-CspSources {
    param(
        [hashtable]$Directives,
        [string]$Name,
        [string[]]$Allowed,
        [string[]]$Required,
        [switch]$AllowNonceOrHash
    )
    Assert-Condition $Directives.ContainsKey($Name) "production CSP is missing: $Name"
    $values = @($Directives[$Name])
    Assert-Condition ($values.Count -gt 0) "production CSP directive has no sources: $Name"
    foreach ($requiredSource in $Required) {
        Assert-Condition ($values -ccontains $requiredSource) "production CSP $Name is missing required source: $requiredSource"
    }
    foreach ($source in $values) {
        if ($Allowed -ccontains $source) {
            continue
        }
        if ($AllowNonceOrHash -and $source -cmatch "^'(?:nonce-[A-Za-z0-9+/_-]+|sha(?:256|384|512)-[A-Za-z0-9+/=]+)'$") {
            continue
        }
        throw "production CSP $Name contains an unapproved source: $source"
    }
}

function Assert-ExecutableContainsUtf8 {
    param([string]$Path, [string]$Expected, [string]$Label)
    $bytes = [IO.File]::ReadAllBytes($Path)
    $utf8 = [Text.Encoding]::UTF8.GetString($bytes)
    $utf16 = [Text.Encoding]::Unicode.GetString($bytes)
    Assert-Condition (
        $utf8.Contains($Expected, [StringComparison]::Ordinal) -or
        $utf16.Contains($Expected, [StringComparison]::Ordinal)
    ) "$Label is not embedded in the packaged executable"
}

function Assert-EmbeddedWindowsManifest {
    param(
        [string]$Path,
        [string]$Label,
        [string]$ManifestTool
    )
    $manifestPath = Join-Path ([IO.Path]::GetTempPath()) ("music-folder-builder-{0}.manifest" -f [Guid]::NewGuid())
    try {
        & $ManifestTool -nologo "-inputresource:$Path;#1" "-out:$manifestPath"
        Assert-Condition ($LASTEXITCODE -eq 0) "mt.exe could not extract the $Label executable manifest"
        [xml]$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8
        $commonControls = @(
            $manifest.SelectNodes("//*[local-name()='assemblyIdentity']") |
                Where-Object { $_.name -eq "Microsoft.Windows.Common-Controls" }
        )
        Assert-Condition ($commonControls.Count -eq 1) "$Label manifest must request Common-Controls exactly once"
        Assert-Condition ($commonControls[0].version -eq "6.0.0.0") "$Label manifest must request Common-Controls v6"
        Assert-Condition ($commonControls[0].publicKeyToken -eq "6595b64144ccf1df") "$Label manifest has an unexpected Common-Controls publisher token"
        $longPathAware = @($manifest.SelectNodes("//*[local-name()='longPathAware']"))
        Assert-Condition ($longPathAware.Count -eq 1) "$Label manifest must declare longPathAware exactly once"
        Assert-Condition ($longPathAware[0].InnerText.Trim() -ceq "true") "$Label manifest must set longPathAware=true"
    }
    finally {
        Remove-Item -LiteralPath $manifestPath -Force -ErrorAction SilentlyContinue
    }
}

$configPath = Join-Path $WorkspaceRoot "crates/desktop/tauri.conf.json"
Assert-Condition (Test-Path -LiteralPath $configPath -PathType Leaf) "Tauri config is missing: $configPath"
$config = Get-Content -LiteralPath $configPath -Raw -Encoding UTF8 | ConvertFrom-Json
Assert-Condition ($config.'$schema' -eq "https://schema.tauri.app/config/2") "tauri.conf.json must use the pinned Tauri v2 schema"

$csp = [string]$config.app.security.csp
Assert-Condition (-not [string]::IsNullOrWhiteSpace($csp)) "production CSP must not be null or empty"
$directives = Get-CspDirectives $csp
$approvedDirectives = @(
    "default-src", "base-uri", "object-src", "frame-ancestors", "form-action",
    "script-src", "style-src", "img-src", "font-src", "connect-src"
)
foreach ($name in $directives.Keys) {
    Assert-Condition ($approvedDirectives -ccontains $name) "production CSP contains an unreviewed directive: $name"
}
Assert-CspSources $directives "default-src" @("'self'") @("'self'")
Assert-CspSources $directives "base-uri" @("'none'") @("'none'")
Assert-CspSources $directives "object-src" @("'none'") @("'none'")
Assert-CspSources $directives "frame-ancestors" @("'none'") @("'none'")
Assert-CspSources $directives "form-action" @("'self'") @("'self'")
Assert-CspSources $directives "script-src" @("'self'") @("'self'") -AllowNonceOrHash
Assert-CspSources $directives "style-src" @("'self'") @("'self'") -AllowNonceOrHash
Assert-CspSources $directives "img-src" @("'self'", "asset:", "http://asset.localhost", "data:") @("'self'")
Assert-CspSources $directives "font-src" @("'self'") @("'self'")
Assert-CspSources $directives "connect-src" @("'self'", "ipc:", "http://ipc.localhost") @("'self'")
Assert-Condition (-not ($csp -cmatch "'unsafe-(?:eval|inline)'")) "production CSP must not allow unsafe-eval or unsafe-inline"

$targets = $config.bundle.targets
$targetText = if ($targets -is [string]) { $targets } else { $targets -join "," }
Assert-Condition (($targetText -eq "all") -or (($targetText -match "msi") -and ($targetText -match "nsis"))) "bundle targets must include both MSI and NSIS"

$distIndex = Join-Path $WorkspaceRoot "ui/dist/index.html"
if (Test-Path -LiteralPath $distIndex -PathType Leaf) {
    $html = Get-Content -LiteralPath $distIndex -Raw -Encoding UTF8
    Assert-Condition (-not ($html -match "(?is)<script(?![^>]*\bsrc\s*=)[^>]*>")) "production UI contains an inline script"
    Assert-Condition (-not ($html -match '(?i)(?:src|href)\s*=\s*[''"]https?://')) "production UI loads a remote script or stylesheet"
}

Write-Host "Tauri schema and production CSP inspection passed"
if ($ConfigOnly) {
    exit 0
}

$resolvedBundle = Resolve-WorkspacePath $BundleDirectory
$resolvedExecutable = Resolve-WorkspacePath $ExecutablePath
$resolvedCliExecutable = Resolve-WorkspacePath $CliExecutablePath
Assert-Condition (Test-Path -LiteralPath $resolvedBundle -PathType Container) "bundle directory is missing: $resolvedBundle"
Assert-Condition (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf) "desktop executable is missing: $resolvedExecutable"
Assert-Condition (Test-Path -LiteralPath $resolvedCliExecutable -PathType Leaf) "CLI executable is missing: $resolvedCliExecutable"
Assert-ExecutableContainsUtf8 $resolvedExecutable $csp "production CSP"

$msi = @(Get-ChildItem -LiteralPath $resolvedBundle -Recurse -File -Filter "*.msi")
$nsis = @(Get-ChildItem -LiteralPath $resolvedBundle -Recurse -File -Filter "*-setup.exe")
Assert-Condition ($msi.Count -gt 0) "Windows bundle does not contain an MSI installer"
Assert-Condition ($nsis.Count -gt 0) "Windows bundle does not contain an NSIS installer"

$mt = Find-WindowsSdkTool "mt.exe"
Assert-EmbeddedWindowsManifest $resolvedExecutable "desktop" $mt
Assert-EmbeddedWindowsManifest $resolvedCliExecutable "CLI" $mt

Write-Host "Windows bundle inspection passed: desktop/CLI Common-Controls and longPathAware manifests, embedded CSP, MSI, and NSIS"
