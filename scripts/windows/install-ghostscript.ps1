# Fetches Ghostscript from Artifex and runs their installer.
#
# Nothing is redistributed here: the file is downloaded from Artifex's own
# release server, so you receive Ghostscript directly from them under their
# licence, which their installer presents. This only automates the download
# you would otherwise do by hand.
#
# The downloaded executable is checked for a valid Artifex Authenticode
# signature BEFORE it is run. Any failure exits non-zero, and the caller then
# falls back to opening Artifex's download page.
$ErrorActionPreference = 'Stop'
# Invoke-WebRequest's progress bar makes large downloads crawl in PS 5.1.
$ProgressPreference = 'SilentlyContinue'

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

    Write-Host 'Looking up the current Ghostscript release from Artifex...'
    $api = 'https://api.github.com/repos/ArtifexSoftware/ghostpdl-downloads/releases/latest'
    $rel = Invoke-RestMethod -UseBasicParsing -Uri $api -Headers @{ 'User-Agent' = 'AI8-Converter' }
    $asset = $rel.assets | Where-Object { $_.name -match '^gs\d+w64\.exe$' } | Select-Object -First 1
    if (-not $asset) { throw 'that release has no 64-bit Windows installer' }

    $dest = Join-Path $env:TEMP $asset.name
    Write-Host ('Downloading {0} ({1:N0} MB)...' -f $asset.name, ($asset.size / 1MB))
    Invoke-WebRequest -UseBasicParsing -Uri $asset.browser_download_url -OutFile $dest

    # Never execute it unless Artifex really signed it.
    $sig = Get-AuthenticodeSignature -FilePath $dest
    if ($sig.Status -ne 'Valid') { throw "the download's signature is $($sig.Status)" }
    if ($sig.SignerCertificate.Subject -notmatch 'Artifex') {
        throw "unexpected signer $($sig.SignerCertificate.Subject)"
    }
    Write-Host 'Signature verified as Artifex Software.'
    Write-Host ''
    Write-Host "Starting Artifex's installer - accept their licence to continue."
    $p = Start-Process -FilePath $dest -Wait -PassThru
    Remove-Item $dest -Force -ErrorAction SilentlyContinue
    exit $p.ExitCode
}
catch {
    Write-Host ''
    Write-Host "Could not fetch it from Artifex: $($_.Exception.Message)"
    exit 1
}
