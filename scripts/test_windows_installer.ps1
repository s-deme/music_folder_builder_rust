[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$BundleDirectory,
    [string]$ExpectedExecutable = "music-folder-desktop.exe",
    [string]$WorkspaceRoot = (Split-Path -Parent $PSScriptRoot),
    [ValidateSet("None", "Present", "Trusted")]
    [string]$SignaturePolicy = "None",
    [string]$ExpectedSignerSubject
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Find-WindowsSdkTool {
    param([string]$Name)
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits/10/bin"
    $candidate = Get-ChildItem -LiteralPath $sdkRoot -Directory |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName "x64/$Name" } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($candidate)) "$Name was not found in the Windows SDK"
    return $candidate
}

function Assert-PackagedExecutable {
    param(
        [string]$Path,
        [string]$ExpectedCsp,
        [string]$ManifestTool,
        [string]$SignatureTool,
        [string]$RequiredSignature,
        [string]$RequiredSigner
    )
    $bytes = [IO.File]::ReadAllBytes($Path)
    $utf8 = [Text.Encoding]::UTF8.GetString($bytes)
    $utf16 = [Text.Encoding]::Unicode.GetString($bytes)
    Assert-Condition (
        $utf8.Contains($ExpectedCsp, [StringComparison]::Ordinal) -or
        $utf16.Contains($ExpectedCsp, [StringComparison]::Ordinal)
    ) "installed executable does not contain the approved production CSP: $Path"

    $manifestPath = Join-Path ([IO.Path]::GetTempPath()) ("music-folder-installer-{0}.manifest" -f [Guid]::NewGuid())
    try {
        & $ManifestTool -nologo "-inputresource:$Path;#1" "-out:$manifestPath"
        Assert-Condition ($LASTEXITCODE -eq 0) "could not extract installed executable manifest: $Path"
        $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8
        Assert-Condition ($manifest -match "(?is)<(?:\w+:)?longPathAware(?:\s[^>]*)?>\s*true\s*</(?:\w+:)?longPathAware>") "installed executable manifest must set longPathAware=true: $Path"
    }
    finally {
        Remove-Item -LiteralPath $manifestPath -Force -ErrorAction SilentlyContinue
    }
    Assert-ArtifactSignature $Path $SignatureTool $RequiredSignature $RequiredSigner
}

function Assert-ArtifactSignature {
    param([string]$Path, [string]$SignatureTool, [string]$Policy, [string]$SignerSubject)
    if ($Policy -eq "None") {
        return
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    Assert-Condition ($null -ne $signature.SignerCertificate) "artifact has no Authenticode signer: $Path"
    Assert-Condition ($signature.Status -ne [Management.Automation.SignatureStatus]::NotSigned) "artifact is unsigned: $Path"
    Assert-Condition ($signature.Status -ne [Management.Automation.SignatureStatus]::HashMismatch) "artifact signature hash mismatch: $Path"
    if (-not [string]::IsNullOrWhiteSpace($SignerSubject)) {
        Assert-Condition ($signature.SignerCertificate.Subject -eq $SignerSubject) "unexpected artifact signer: $Path"
    }
    if ($Policy -eq "Trusted") {
        & $SignatureTool verify /pa /all $Path
        Assert-Condition ($LASTEXITCODE -eq 0) "artifact signature is not trusted: $Path"
    }
}

$bundle = [IO.Path]::GetFullPath($BundleDirectory)
Assert-Condition (Test-Path -LiteralPath $bundle -PathType Container) "bundle directory is missing: $bundle"
$msi = Get-ChildItem -LiteralPath $bundle -Recurse -File -Filter "*.msi" | Select-Object -First 1
$nsis = Get-ChildItem -LiteralPath $bundle -Recurse -File -Filter "*-setup.exe" | Select-Object -First 1
Assert-Condition ($null -ne $msi) "MSI installer is missing"
Assert-Condition ($null -ne $nsis) "NSIS installer is missing"
$configPath = Join-Path $WorkspaceRoot "crates/desktop/tauri.conf.json"
$config = Get-Content -LiteralPath $configPath -Raw -Encoding UTF8 | ConvertFrom-Json
$productionCsp = [string]$config.app.security.csp
Assert-Condition (-not [string]::IsNullOrWhiteSpace($productionCsp)) "production CSP is missing"
$mt = Find-WindowsSdkTool "mt.exe"
$signtool = Find-WindowsSdkTool "signtool.exe"
Assert-ArtifactSignature $msi.FullName $signtool $SignaturePolicy $ExpectedSignerSubject
Assert-ArtifactSignature $nsis.FullName $signtool $SignaturePolicy $ExpectedSignerSubject

$scratch = Join-Path ([IO.Path]::GetTempPath()) ("music-folder-installer-smoke-{0}" -f [Guid]::NewGuid())
$msiTarget = Join-Path $scratch "msi"
$nsisTarget = Join-Path $scratch "nsis"
New-Item -ItemType Directory -Path $msiTarget, $nsisTarget -Force | Out-Null

try {
    $msiProcess = Start-Process -FilePath "msiexec.exe" -ArgumentList @(
        "/a",
        ('"{0}"' -f $msi.FullName),
        "/qn",
        ('TARGETDIR="{0}"' -f $msiTarget)
    ) -Wait -PassThru
    Assert-Condition ($msiProcess.ExitCode -eq 0) "MSI administrative extraction failed with exit code $($msiProcess.ExitCode)"
    $msiExecutable = Get-ChildItem -LiteralPath $msiTarget -Recurse -File -Filter $ExpectedExecutable | Select-Object -First 1
    Assert-Condition ($null -ne $msiExecutable) "MSI extraction does not contain $ExpectedExecutable"
    Assert-PackagedExecutable $msiExecutable.FullName $productionCsp $mt $signtool $SignaturePolicy $ExpectedSignerSubject

    $nsisProcess = Start-Process -FilePath $nsis.FullName -ArgumentList @(
        "/S",
        "/D=$nsisTarget"
    ) -Wait -PassThru
    Assert-Condition ($nsisProcess.ExitCode -eq 0) "NSIS silent install failed with exit code $($nsisProcess.ExitCode)"
    $nsisExecutable = Get-ChildItem -LiteralPath $nsisTarget -Recurse -File -Filter $ExpectedExecutable | Select-Object -First 1
    Assert-Condition ($null -ne $nsisExecutable) "NSIS silent install does not contain $ExpectedExecutable"
    Assert-PackagedExecutable $nsisExecutable.FullName $productionCsp $mt $signtool $SignaturePolicy $ExpectedSignerSubject

    $uninstaller = Get-ChildItem -LiteralPath $nsisTarget -Recurse -File |
        Where-Object { $_.Name -match "(?i)^unins.*\.exe$|uninstall.*\.exe$" } |
        Select-Object -First 1
    Assert-Condition ($null -ne $uninstaller) "NSIS silent install did not create an uninstaller"
    $uninstallProcess = Start-Process -FilePath $uninstaller.FullName -ArgumentList "/S" -Wait -PassThru
    Assert-Condition ($uninstallProcess.ExitCode -eq 0) "NSIS silent uninstall failed with exit code $($uninstallProcess.ExitCode)"
}
finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "MSI/NSIS install smoke passed with packaged CSP and longPathAware inspection"
