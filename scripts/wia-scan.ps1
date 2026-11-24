# WIA scanner acquisition sidecar for `pdfxi create scan --device`.
#
# Enumerates WIA devices through COM, connects to the first device (or the one
# named by -DeviceName), acquires one image per page into -OutDir as PNG and
# prints {"device": "...", "pages": ["...", ...]}. Exit 3 when no scanner is
# attached so the CLI can report a typed hardware blocker instead of a generic
# failure.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutDir,
    [string]$DeviceName = ''
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

try {
    $manager = New-Object -ComObject WIA.DeviceManager
} catch {
    [Console]::Error.WriteLine("WIA is not available: $($_.Exception.Message)")
    exit 3
}
$infos = @($manager.DeviceInfos)
if ($infos.Count -eq 0) {
    [Console]::Error.WriteLine('no WIA scanner is attached to this machine')
    exit 3
}
$info = $infos | Where-Object {
    -not $DeviceName -or $_.Properties.Item('Name').Value -like "*$DeviceName*"
} | Select-Object -First 1
if ($null -eq $info) {
    [Console]::Error.WriteLine("no WIA scanner named '$DeviceName'")
    exit 3
}
$device = $null
try {
    $device = $info.Connect()
} catch {
    [Console]::Error.WriteLine("no WIA scanner: the device is unavailable: $($_.Exception.Message)")
    exit 3
}
$name = $info.Properties.Item('Name').Value

$pngFormat = '{B96B3CAE-0728-11D3-9D7B-0000F81EF32E}'
Add-Type -AssemblyName System.Drawing
$pages = New-Object System.Collections.Generic.List[string]
for ($index = 1; $index -le $device.Items.Count; $index++) {
    $item = $device.Items.Item($index)
    $image = $item.Transfer($pngFormat)
    if ($null -eq $image) {
        break
    }
    $raw = Join-Path $OutDir ("scan-{0}.raw" -f $index)
    $image.SaveFile($raw)
    $png = Join-Path $OutDir ("scan-{0}.png" -f $index)
    $header = [System.IO.File]::ReadAllBytes($raw)[0..7]
    $isPng = ($header[0] -eq 0x89 -and $header[1] -eq 0x50 -and $header[2] -eq 0x4E -and $header[3] -eq 0x47)
    if ($isPng) {
        Move-Item -LiteralPath $raw -Destination $png -Force
    } else {
        # Many WIA drivers ignore the requested format and transfer BMP; convert
        # it so the CLI's PNG-only scan assembly accepts the page.
        try {
            $bitmap = [System.Drawing.Image]::FromFile($raw)
            $bitmap.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
            $bitmap.Dispose()
        } catch {
            Remove-Item -LiteralPath $raw -Force -ErrorAction SilentlyContinue
            throw "the scanner returned an unsupported image format: $($_.Exception.Message)"
        }
        Remove-Item -LiteralPath $raw -Force -ErrorAction SilentlyContinue
    }
    $pages.Add($png)
    if (-not $device.Properties.Exists('DocumentHandlingSelect') -or
        $device.Items.Count -le 1) {
        break
    }
}
if ($pages.Count -eq 0) {
    [Console]::Error.WriteLine('the scanner returned no pages')
    exit 3
}
[ordered]@{ device = $name; pages = $pages } | ConvertTo-Json -Depth 3 -Compress
