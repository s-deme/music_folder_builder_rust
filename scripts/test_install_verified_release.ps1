$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'install_verified_release.ps1') -LibraryOnly

function Assert-Equal($Expected, $Actual, [string]$Message) {
    if ($Expected -ne $Actual) { throw "$Message expected=$Expected actual=$Actual" }
}

Assert-Equal 0 (Compare-StrictSemVer '1.2.3' '1.2.3') 'equal versions'
Assert-Equal 1 (Compare-StrictSemVer '1.2.4' '1.2.3') 'patch increase'
Assert-Equal 1 (Compare-StrictSemVer '2.0.0' '1.99.99') 'major increase'
Assert-Equal -1 (Compare-StrictSemVer '1.2.3-alpha.1' '1.2.3-alpha.2') 'prerelease numeric order'
Assert-Equal 1 (Compare-StrictSemVer '1.2.3' '1.2.3-rc.1') 'release after prerelease'
Assert-MonotonicReleaseVersion -Candidate '1.0.1' -Installed '1.0.0'

$rejected = $false
try { Assert-MonotonicReleaseVersion -Candidate '1.0.0' -Installed '1.0.0' } catch { $rejected = $true }
if (-not $rejected) { throw 'equal version must be rejected' }
$rejected = $false
try { Assert-MonotonicReleaseVersion -Candidate '0.9.9' -Installed '1.0.0' } catch { $rejected = $true }
if (-not $rejected) { throw 'downgrade must be rejected' }
$rejected = $false
try { ConvertFrom-StrictSemVer '01.2.3' } catch { $rejected = $true }
if (-not $rejected) { throw 'non-canonical semantic version must be rejected' }
foreach ($invalid in @('v1.2.3', 'vvvv1.2.3', '1.2.3-alpha..1', '1.2.3-01', '1.2.3+build..1')) {
    $rejected = $false
    try { ConvertFrom-StrictSemVer $invalid } catch { $rejected = $true }
    if (-not $rejected) { throw "invalid semantic version was accepted: $invalid" }
}
$rejected = $false
try { Assert-TrustedLauncherSignature -LiteralPath $PSCommandPath -Publisher 'CN=Untrusted Test' } catch { $rejected = $true }
if (-not $rejected) { throw 'unsigned checkout launcher must be rejected by self-signature policy' }

Write-Host 'verified installer policy unit tests passed'
