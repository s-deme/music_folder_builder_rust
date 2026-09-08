param(
    [string]$ReleaseDirectory,
    [string]$InstallerName,
    [string]$ExpectedPublisher,
    [switch]$VerifyOnly,
    [switch]$LibraryOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function ConvertFrom-StrictSemVer {
    param([Parameter(Mandatory = $true)][string]$Version)
    $normalized = $Version
    if ($normalized -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$') {
        throw "invalid semantic version: $Version"
    }
    $major = $Matches[1]
    $minor = $Matches[2]
    $patch = $Matches[3]
    $prerelease = $Matches[4]
    if (-not [string]::IsNullOrEmpty($prerelease)) {
        foreach ($identifier in $prerelease.Split('.')) {
            if ($identifier -match '^\d+$' -and $identifier.Length -gt 1 -and $identifier.StartsWith('0', [StringComparison]::Ordinal)) {
                throw "numeric prerelease identifier has a leading zero: $Version"
            }
        }
    }
    [pscustomobject]@{
        Major = [uint64]$major
        Minor = [uint64]$minor
        Patch = [uint64]$patch
        PreRelease = $prerelease
        Text = $normalized
    }
}

function Get-InstalledProductVersion {
    param([Parameter(Mandatory = $true)][string]$ProductName)
    $registryRoots = @(
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    $versions = @()
    foreach ($root in $registryRoots) {
        foreach ($entry in @(Get-ItemProperty -Path $root -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq $ProductName })) {
            if ([string]::IsNullOrWhiteSpace([string]$entry.DisplayVersion)) { continue }
            [void](ConvertFrom-StrictSemVer ([string]$entry.DisplayVersion))
            $versions += [string]$entry.DisplayVersion
        }
    }
    if ($versions.Count -eq 0) { return '0.0.0' }
    $highest = $versions[0]
    foreach ($version in $versions | Select-Object -Skip 1) {
        if ((Compare-StrictSemVer $version $highest) -gt 0) { $highest = $version }
    }
    return $highest
}

function Compare-PreReleaseIdentifier {
    param([string]$Left, [string]$Right)
    $leftNumeric = $Left -match '^\d+$'
    $rightNumeric = $Right -match '^\d+$'
    if ($leftNumeric -and $rightNumeric) {
        $leftNumber = [uint64]$Left
        $rightNumber = [uint64]$Right
        return [Math]::Sign($leftNumber.CompareTo($rightNumber))
    }
    if ($leftNumeric) { return -1 }
    if ($rightNumeric) { return 1 }
    return [Math]::Sign([string]::CompareOrdinal($Left, $Right))
}

function Compare-StrictSemVer {
    param(
        [Parameter(Mandatory = $true)][string]$Left,
        [Parameter(Mandatory = $true)][string]$Right
    )
    $leftVersion = ConvertFrom-StrictSemVer $Left
    $rightVersion = ConvertFrom-StrictSemVer $Right
    foreach ($field in @("Major", "Minor", "Patch")) {
        $comparison = $leftVersion.$field.CompareTo($rightVersion.$field)
        if ($comparison -ne 0) { return [Math]::Sign($comparison) }
    }
    if ([string]::IsNullOrEmpty($leftVersion.PreRelease) -and [string]::IsNullOrEmpty($rightVersion.PreRelease)) { return 0 }
    if ([string]::IsNullOrEmpty($leftVersion.PreRelease)) { return 1 }
    if ([string]::IsNullOrEmpty($rightVersion.PreRelease)) { return -1 }
    $leftParts = $leftVersion.PreRelease.Split('.')
    $rightParts = $rightVersion.PreRelease.Split('.')
    for ($index = 0; $index -lt [Math]::Min($leftParts.Count, $rightParts.Count); $index++) {
        $comparison = Compare-PreReleaseIdentifier $leftParts[$index] $rightParts[$index]
        if ($comparison -ne 0) { return $comparison }
    }
    return [Math]::Sign($leftParts.Count.CompareTo($rightParts.Count))
}

function Assert-MonotonicReleaseVersion {
    param(
        [Parameter(Mandatory = $true)][string]$Candidate,
        [Parameter(Mandatory = $true)][string]$Installed
    )
    if ((Compare-StrictSemVer $Candidate $Installed) -le 0) {
        throw "release version must increase monotonically: installed=$Installed candidate=$Candidate"
    }
}

function Get-Sha256Lower {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)
    (Get-FileHash -LiteralPath $LiteralPath -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-TrustedLauncherSignature {
    param(
        [Parameter(Mandatory = $true)][string]$LiteralPath,
        [Parameter(Mandatory = $true)][string]$Publisher
    )
    $signature = Get-AuthenticodeSignature -LiteralPath $LiteralPath
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
        throw "release launcher Authenticode signature is not trusted: $($signature.Status)"
    }
    if ($null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Subject -ne $Publisher) {
        throw "release launcher publisher differs from trusted policy"
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "release launcher signature has no trusted timestamp"
    }
}

function Resolve-ConfinedReleaseFile {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Name
    )
    if ([string]::IsNullOrWhiteSpace($Name) -or $Name -match '[/\\]' -or $Name -in @('.', '..') -or [IO.Path]::GetFileName($Name) -ne $Name) {
        throw "installer name must be a basename"
    }
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $candidate = [IO.Path]::GetFullPath((Join-Path $rootPath $Name))
    if (-not $candidate.StartsWith($rootPath, [StringComparison]::OrdinalIgnoreCase)) {
        throw "installer escapes the release directory"
    }
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "installer is missing: $Name"
    }
    $candidate
}

function Get-InstallerProductVersion {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)
    if ([IO.Path]::GetExtension($LiteralPath).Equals('.msi', [StringComparison]::OrdinalIgnoreCase)) {
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $database = $null
        $view = $null
        try {
            $database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($LiteralPath, 0))
            $view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database, @("SELECT Value FROM Property WHERE Property='ProductVersion'"))
            $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
            $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
            if ($null -eq $record) { throw "MSI ProductVersion is missing" }
            return [string]$record.StringData(1)
        }
        finally {
            if ($null -ne $view) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null }
            if ($null -ne $database) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($database) | Out-Null }
            [Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
        }
    }
    $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($LiteralPath).ProductVersion
    if ([string]::IsNullOrWhiteSpace($version)) { throw "installer ProductVersion is missing" }
    $version.Split('+')[0]
}

function Test-VerifiedRelease {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Publisher,
        [Parameter(Mandatory = $true)][string]$CurrentVersion
    )
    $manifestPath = Join-Path $Directory 'release-manifest.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "release-manifest.json is missing" }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schema_version -ne 1) { throw "unsupported release manifest schema" }
    if ($manifest.channel -ne 'production-signed') { throw "only production-signed channel can be installed" }
    if ($manifest.product -ne 'Music Folder Builder') { throw "release manifest product identity differs" }
    if ($manifest.source_commit -notmatch '^[0-9a-f]{40}([0-9a-f]{24})?$') { throw "manifest source commit is invalid" }
    Assert-MonotonicReleaseVersion -Candidate ([string]$manifest.version) -Installed $CurrentVersion
    $artifact = @($manifest.artifacts | Where-Object { $_.name -eq $Name })
    if ($artifact.Count -ne 1 -or $artifact[0].kind -notin @('msi', 'nsis')) { throw "installer is not uniquely declared in manifest" }
    $installerPath = Resolve-ConfinedReleaseFile -Root $Directory -Name $Name
    if ((Get-Item -LiteralPath $installerPath).Length -ne [int64]$artifact[0].size) { throw "installer size differs from manifest" }
    $actualDigest = Get-Sha256Lower -LiteralPath $installerPath
    if ($actualDigest -ne [string]$artifact[0].sha256) { throw "installer checksum differs from manifest" }
    $checksumLine = Get-Content -LiteralPath (Join-Path $Directory 'INSTALLER-SHA256SUMS') | Where-Object { $_ -match "^[0-9a-f]{64} [ *]$([regex]::Escape($Name))$" }
    if (@($checksumLine).Count -ne 1 -or -not $checksumLine[0].StartsWith($actualDigest, [StringComparison]::Ordinal)) {
        throw "installer checksum list does not bind the selected artifact"
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $installerPath
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) { throw "installer Authenticode signature is not trusted: $($signature.Status)" }
    if ($null -eq $signature.SignerCertificate -or $signature.SignerCertificate.Subject -ne $Publisher) { throw "installer publisher differs from trusted policy" }
    if ($null -eq $signature.TimeStamperCertificate) { throw "installer signature has no trusted timestamp" }
    $binaryVersion = Get-InstallerProductVersion -LiteralPath $installerPath
    if ((Compare-StrictSemVer $binaryVersion ([string]$manifest.version)) -ne 0) { throw "signed installer version differs from release manifest" }
    [pscustomobject]@{
        InstallerPath = $installerPath
        Version = [string]$manifest.version
        Channel = [string]$manifest.channel
        Sha256 = $actualDigest
        Publisher = $signature.SignerCertificate.Subject
    }
}

if ($LibraryOnly) { return }
if ([string]::IsNullOrWhiteSpace($ReleaseDirectory) -or [string]::IsNullOrWhiteSpace($InstallerName) -or [string]::IsNullOrWhiteSpace($ExpectedPublisher)) {
    throw "-ReleaseDirectory, -InstallerName, and -ExpectedPublisher are required"
}
Assert-TrustedLauncherSignature -LiteralPath $PSCommandPath -Publisher $ExpectedPublisher
$installedVersion = Get-InstalledProductVersion -ProductName 'Music Folder Builder'
$verified = Test-VerifiedRelease -Directory $ReleaseDirectory -Name $InstallerName -Publisher $ExpectedPublisher -CurrentVersion $installedVersion
Write-Host "verified production installer version=$($verified.Version) sha256=$($verified.Sha256) publisher=$($verified.Publisher)"
if (-not $VerifyOnly) {
    if ([IO.Path]::GetExtension($verified.InstallerPath).Equals('.msi', [StringComparison]::OrdinalIgnoreCase)) {
        $quotedInstaller = '"{0}"' -f $verified.InstallerPath.Replace('"', '""')
        $process = Start-Process -FilePath msiexec.exe -ArgumentList @('/i', $quotedInstaller, '/norestart') -Wait -PassThru
    }
    else {
        $process = Start-Process -FilePath $verified.InstallerPath -Wait -PassThru
    }
    if ($process.ExitCode -ne 0) { throw "installer failed with exit code $($process.ExitCode)" }
}
