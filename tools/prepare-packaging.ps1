[CmdletBinding()]
param([string]$CargoPath)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$project=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$support=Join-Path $PSScriptRoot 'portable-support'
function Download-NewOrVerified([string]$Url,[string]$Path,[string]$Hash) {
    if (Test-Path -LiteralPath $Path) {
        if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Hash) { throw 'An existing download has a different checksum; preserve it and choose a new cache location.' }
        return
    }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    $temporary=$Path+'.download-'+[guid]::NewGuid().ToString('N')
    Invoke-WebRequest -Uri $Url -OutFile $temporary -TimeoutSec 60
    if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $Hash) { throw 'Downloaded checksum differs from the recorded upstream input. Temporary bytes were preserved.' }
    [IO.File]::Move($temporary,$Path)
}
$assets=[IO.File]::ReadAllText((Join-Path $support 'provenance.json')) | ConvertFrom-Json
foreach ($asset in $assets.assets) {
    $target=[IO.Path]::GetFullPath((Join-Path $support $asset.path))
    if (!$target.StartsWith($support+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid support asset path.' }
    Download-NewOrVerified $asset.upstream_url $target $asset.sha256
}
$driverZip=Join-Path $project '.cache\driver\rtl-sdr-blog-V1.4.0-Release.zip'
Download-NewOrVerified 'https://github.com/rtlsdrblog/rtl-sdr-blog/releases/download/V1.4.0/Release.zip' $driverZip '7EF33F1304647F65E5E0FDE43637A73D54F076E91E651A3CECC4F55A17FD9815'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive=[IO.Compression.ZipFile]::OpenRead($driverZip)
try {
    $entries=@($archive.Entries | Where-Object { $_.FullName -match '(^|/)x64/rtlsdr\.dll$' })
    if ($entries.Count -ne 1) { throw 'Unique Windows x64 driver entry was not found.' }
    $target=Join-Path $project 'drivers\rtlsdr\rtlsdr.dll'
    $stream=$entries[0].Open();$buffer=[IO.MemoryStream]::new()
    try { $stream.CopyTo($buffer);$bytes=$buffer.ToArray() } finally { $stream.Dispose();$buffer.Dispose() }
    $sha=[Security.Cryptography.SHA256]::Create()
    try { $expected=[BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-','') } finally { $sha.Dispose() }
    if (Test-Path -LiteralPath $target) {
        if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne $expected) { throw 'An existing driver differs; refusing to overwrite.' }
    } else {
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
        $file=[IO.File]::Open($target,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
        try { $file.Write($bytes,0,$bytes.Length) } finally { $file.Dispose() }
    }
} finally { $archive.Dispose() }
if (!$CargoPath) {
    $candidate=Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path -LiteralPath $candidate) { $CargoPath=$candidate } else { $CargoPath=(Get-Command cargo.exe -ErrorAction Stop).Source }
}
$metadata=& $CargoPath metadata --manifest-path (Join-Path $project 'native\Cargo.toml') --offline --locked --filter-platform x86_64-pc-windows-msvc --format-version 1
if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed. Complete the normal locked build/dependency fetch first.' }
$snapshot=Join-Path $support ('cargo-metadata-windows-'+(Get-Date -Format 'yyyyMMdd-HHmmss-fff')+'.json')
$file=[IO.File]::Open($snapshot,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write)
try { $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($metadata -join "`n"));$file.Write($bytes,0,$bytes.Length) } finally { $file.Dispose() }
@{cargo_metadata_path=$snapshot;driver_path=$target;hardware_operated=$false;usb_driver_installed=$false;application_started=$false} | ConvertTo-Json
