[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [ValidateSet("Unsigned", "Test", "Production", "VerifySigned")]
    [string]$Mode = "Unsigned",
    [ValidateSet("Installers", "Application", "All")]
    [string]$ArtifactSet = "Installers",
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [switch]$SkipTamperTest
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

function Invoke-SignTool {
    param([string]$Tool, [string[]]$Arguments, [string]$Failure)
    & $Tool @Arguments
    Assert-Condition ($LASTEXITCODE -eq 0) $Failure
}

function Invoke-PfxSigning {
    param(
        [string]$Tool,
        [System.Collections.IEnumerable]$Artifacts,
        [string]$CertificatePath,
        [string]$Password,
        [string]$Timestamp,
        [string]$Label
    )
    foreach ($artifact in $Artifacts) {
        $arguments = @(
            "sign", "/f", $CertificatePath, "/p", $Password, "/fd", "SHA256"
        )
        if (-not [string]::IsNullOrWhiteSpace($Timestamp)) {
            $arguments += @("/tr", $Timestamp, "/td", "SHA256")
        }
        $arguments += $artifact.FullName
        Invoke-SignTool $Tool $arguments "$Label signing failed for $($artifact.Name)"
    }
}

$inputRoot = [IO.Path]::GetFullPath($InputDirectory)
$outputRoot = [IO.Path]::GetFullPath($OutputDirectory)
Assert-Condition (Test-Path -LiteralPath $inputRoot -PathType Container) "input directory is missing: $inputRoot"
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
Assert-Condition (@(Get-ChildItem -LiteralPath $outputRoot -Force).Count -eq 0) "output directory must be empty: $outputRoot"

$sources = @(Get-ChildItem -LiteralPath $inputRoot -Recurse -File | Where-Object {
    $isInstaller = $_.Extension -eq ".msi" -or $_.Name -match "(?i)-setup\.exe$"
    $isApplication = $_.Extension -eq ".exe" -and $_.Name -notmatch "(?i)-setup\.exe$"
    switch ($ArtifactSet) {
        "Installers" { $isInstaller }
        "Application" { $isApplication }
        "All" { $isInstaller -or $isApplication }
    }
})
Assert-Condition ($sources.Count -gt 0) "no artifacts matched ArtifactSet=$ArtifactSet"
if ($ArtifactSet -eq "Application") {
    Assert-Condition ($sources.Count -eq 1) "Application selection must contain exactly one executable"
}
if ($ArtifactSet -eq "Installers" -or $ArtifactSet -eq "All") {
    Assert-Condition (@($sources | Where-Object Extension -eq ".msi").Count -gt 0) "artifact set has no MSI"
    Assert-Condition (@($sources | Where-Object Name -Match "(?i)-setup\.exe$").Count -gt 0) "artifact set has no NSIS setup executable"
}
if ($ArtifactSet -eq "All") {
    Assert-Condition (@($sources | Where-Object { $_.Extension -eq ".exe" -and $_.Name -notmatch "(?i)-setup\.exe$" }).Count -eq 1) "All selection must contain exactly one application executable"
}
$artifacts = @()
foreach ($source in $sources) {
    $destination = Join-Path $outputRoot $source.Name
    Assert-Condition (-not (Test-Path -LiteralPath $destination)) "duplicate installer filename: $($source.Name)"
    Copy-Item -LiteralPath $source.FullName -Destination $destination
    $artifacts += Get-Item -LiteralPath $destination
}

if ($Mode -eq "Unsigned") {
    foreach ($artifact in $artifacts) {
        $signature = Get-AuthenticodeSignature -LiteralPath $artifact.FullName
        Assert-Condition ($signature.Status -eq [Management.Automation.SignatureStatus]::NotSigned) "unsigned channel contains a signed artifact: $($artifact.Name)"
    }
    Write-Host "unsigned artifact policy verified for $($artifacts.Count) installer(s)"
    exit 0
}

$signtool = Find-WindowsSdkTool "signtool.exe"
$testCertificate = $null
$certificateFile = $null
$testPfxFile = $null
$testPfxPassword = $null
try {
    if ($Mode -eq "Test") {
        $testPfxPassword = "ci-test-{0}" -f [Guid]::NewGuid()
        $testCertificate = New-SelfSignedCertificate `
            -Type CodeSigningCert `
            -Subject "CN=Music Folder Builder CI Test Only" `
            -CertStoreLocation "Cert:\CurrentUser\My" `
            -KeyExportPolicy Exportable `
            -NotAfter (Get-Date).AddDays(1)
        $certificateFile = Join-Path ([IO.Path]::GetTempPath()) ("music-folder-test-signing-{0}.cer" -f [Guid]::NewGuid())
        $testPfxFile = Join-Path ([IO.Path]::GetTempPath()) ("music-folder-test-signing-{0}.pfx" -f [Guid]::NewGuid())
        $securePassword = ConvertTo-SecureString -String $testPfxPassword -AsPlainText -Force
        Export-Certificate -Cert $testCertificate -FilePath $certificateFile | Out-Null
        Export-PfxCertificate -Cert $testCertificate -FilePath $testPfxFile -Password $securePassword -ChainOption EndEntityCertOnly | Out-Null
        Import-Certificate -FilePath $certificateFile -CertStoreLocation "Cert:\CurrentUser\Root" | Out-Null
        Import-Certificate -FilePath $certificateFile -CertStoreLocation "Cert:\CurrentUser\TrustedPublisher" | Out-Null

        $passwordProbe = Join-Path $outputRoot ("wrong-password-probe-{0}" -f $artifacts[0].Name)
        Copy-Item -LiteralPath $artifacts[0].FullName -Destination $passwordProbe
        & $signtool sign /f $testPfxFile /p "deliberately-wrong" /fd SHA256 $passwordProbe 2>$null | Out-Null
        Assert-Condition ($LASTEXITCODE -ne 0) "test PFX unexpectedly accepted an incorrect password"
        Remove-Item -LiteralPath $passwordProbe -Force

        Invoke-PfxSigning $signtool $artifacts $testPfxFile $testPfxPassword "" "test PFX"
    }
    elseif ($Mode -eq "Production") {
        Assert-Condition (-not [string]::IsNullOrWhiteSpace($PfxPath)) "production signing requires -PfxPath"
        Assert-Condition (Test-Path -LiteralPath $PfxPath -PathType Leaf) "production PFX file is missing"
        Assert-Condition (-not [string]::IsNullOrWhiteSpace($PfxPassword)) "production signing requires -PfxPassword"
        Assert-Condition (-not [string]::IsNullOrWhiteSpace($TimestampUrl)) "production signing requires -TimestampUrl"
        Invoke-PfxSigning $signtool $artifacts $PfxPath $PfxPassword $TimestampUrl "production PFX"
    }

    foreach ($artifact in $artifacts) {
        Invoke-SignTool $signtool @("verify", "/pa", "/all", $artifact.FullName) "signature verification failed for $($artifact.Name)"
    }

    if ($Mode -eq "Test" -and -not $SkipTamperTest) {
        $original = $artifacts[0]
        $tampered = Join-Path $outputRoot ("tampered-{0}" -f $original.Name)
        Copy-Item -LiteralPath $original.FullName -Destination $tampered
        $stream = [IO.File]::Open($tampered, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        try {
            $position = [Math]::Max(1, [Math]::Floor($stream.Length / 2))
            [void]$stream.Seek($position, [IO.SeekOrigin]::Begin)
            $value = $stream.ReadByte()
            Assert-Condition ($value -ge 0) "could not read test-signed artifact for tamper test"
            [void]$stream.Seek($position, [IO.SeekOrigin]::Begin)
            $stream.WriteByte($value -bxor 1)
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
        & $signtool verify /pa /all $tampered 2>$null | Out-Null
        Assert-Condition ($LASTEXITCODE -ne 0) "tampered test-signed artifact unexpectedly passed signature verification"
        Remove-Item -LiteralPath $tampered -Force
    }
}
finally {
    if ($null -ne $testCertificate) {
        foreach ($store in @("My", "Root", "TrustedPublisher")) {
            Remove-Item -LiteralPath ("Cert:\CurrentUser\{0}\{1}" -f $store, $testCertificate.Thumbprint) -Force -ErrorAction SilentlyContinue
        }
    }
    if ($null -ne $certificateFile) {
        Remove-Item -LiteralPath $certificateFile -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $testPfxFile) {
        Remove-Item -LiteralPath $testPfxFile -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "$Mode signature verification passed for $($artifacts.Count) installer(s)"
