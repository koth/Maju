#Requires -Version 5.1
<#
.SYNOPSIS
    Regenerate the mobile app icon so it matches the desktop (PC) app icon.

.DESCRIPTION
    Windows twin of scripts/generate-mobile-icons.sh (which needs macOS `sips`).
    Same brand mark, same sizes, same outputs: keep the two in lockstep, so any
    change to one belongs in the other.

    Canonical brand mark: apps/desktop/src-tauri/icons/maju.png (2000x2000, RGBA).
    `tauri icon` regenerates every desktop artifact from it; this script derives
    the Expo source icon plus the prebuild-managed Android mipmaps from the same
    file, so the two platforms can never drift apart again.

    Outputs (re-run after the brand mark changes, then rebuild the app):
      apps/mobile/assets/icon.png                        1024x1024 Expo source
      apps/mobile/android/app/src/main/res/mipmap-*/     legacy + adaptive layers

    Mirrors @expo/prebuild-config's withAndroidIcons: legacy ic_launcher is
    48dp * scale, ic_launcher_foreground is 108dp * scale, both resizeMode cover.
    android.adaptiveIcon is not configured in app.config.ts, so this script also
    removes any round/background layers prebuild would delete.

    Resizing uses GDI+ (part of Windows), so no image toolchain is required for
    the PNG. WebP layers need an encoder; the first one found is used:
    cwebp (libwebp), ImageMagick, ffmpeg. All three are asked for the same
    quality, but their bytes differ — compare the artifacts by eye, not by hash.

.EXAMPLE
    pwsh -File scripts/generate-mobile-icons.ps1

.EXAMPLE
    # Different brand mark (e.g. to preview a candidate before committing it).
    pwsh -File scripts/generate-mobile-icons.ps1 -Source C:\tmp\candidate.png

.NOTES
    The Expo source icon keeps its transparency; iOS flattening happens at
    prebuild time, exactly as in the .sh script.
#>
[CmdletBinding()]
param(
    # Brand mark to derive every mobile icon from. Defaults to the desktop one.
    [string]$Source
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# GDI+ resizing is Windows-only, and so is this script: the macOS twin covers
# everything else.
# (The `$IsWindows` check is guarded because it does not exist in Windows
# PowerShell 5.1, where the short-circuit leaves it unevaluated.)
if ($PSVersionTable.PSEdition -eq 'Core' -and -not $IsWindows) {
    throw 'Windows only — use scripts/generate-mobile-icons.sh on macOS instead'
}

$Root = Split-Path -Parent $PSScriptRoot
if (-not $Source) {
    $Source = Join-Path $Root 'apps\desktop\src-tauri\icons\maju.png'
}
$Mobile = Join-Path $Root 'apps\mobile'
$Res = Join-Path $Mobile 'android\app\src\main\res'
$ExpoIcon = Join-Path $Mobile 'assets\icon.png'

$Quality = 90
$ExpoIconSize = 1024
$Densities = @('mdpi', 'hdpi', 'xhdpi', 'xxhdpi', 'xxxhdpi')
$LegacySizes = @(48, 72, 96, 144, 192)
$ForegroundSizes = @(108, 162, 216, 324, 432)

if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
    throw "missing brand mark: $Source"
}
# Absolute: GDI+'s FromFile and the encoders resolve relative paths against the
# process CWD, which is not necessarily where the script was started from.
$Source = (Resolve-Path -LiteralPath $Source).Path

function Get-WebpEncoder {
    # `Get-Command` reports the executable name (`ffmpeg.exe`), so return the
    # bare tool name explicitly instead of switching on that.
    foreach ($name in 'cwebp', 'magick', 'ffmpeg') {
        $command = Get-Command $name -ErrorAction SilentlyContinue
        if ($command) {
            return [pscustomobject]@{ Kind = $name; Path = $command.Source }
        }
    }
    throw 'no WebP encoder on PATH — install one of: libwebp (cwebp), ImageMagick (magick), ffmpeg (winget install Gyan.FFmpeg)'
}

Add-Type -AssemblyName System.Drawing

# GDI+ resize. `SourceCopy` (not the default blend) matters: it overwrites the
# destination including alpha, so transparency survives instead of compositing
# onto black. Used for the PNG and, on the cwebp path, as cwebp cannot resize.
function New-ResizedPng {
    param(
        [Parameter(Mandatory)][string]$ImagePath,
        [Parameter(Mandatory)][int]$Size,
        [Parameter(Mandatory)][string]$Destination
    )

    $image = [System.Drawing.Image]::FromFile($ImagePath)
    try {
        $bitmap = New-Object System.Drawing.Bitmap $Size, $Size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
                $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
                $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
                $graphics.DrawImage($image, (New-Object System.Drawing.Rectangle 0, 0, $Size, $Size))
            } finally { $graphics.Dispose() }
            $bitmap.Save($Destination, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $bitmap.Dispose() }
    } finally { $image.Dispose() }
}

function Write-WebpIcon {
    param(
        [Parameter(Mandatory)][int]$Size,
        [Parameter(Mandatory)][string]$Destination
    )

    # `$LASTEXITCODE` is read right after the native call in every branch (it is
    # only set once a native command has actually run, and StrictMode throws on
    # reading it otherwise).
    $exit = 0
    switch ($Encoder.Kind) {
        'cwebp' {
            $resized = Join-Path $Scratch "$Size.png"
            New-ResizedPng -ImagePath $Source -Size $Size -Destination $resized
            # `-alpha_q 100` is libwebp's own default; pass it so the intent is
            # explicit and stays equivalent to the macOS script.
            & $Encoder.Path -q $Quality -alpha_q 100 -quiet $resized -o $Destination
            $exit = $LASTEXITCODE
        }
        'magick' {
            & $Encoder.Path $Source -resize "${Size}x${Size}!" -strip -quality $Quality -define webp:alpha-quality=100 $Destination
            $exit = $LASTEXITCODE
        }
        'ffmpeg' {
            # `-preset icon` tunes libwebp for small colourful images. ffmpeg's
            # libwebp encoder has no alpha-quality knob (libwebp's default 100
            # is used), and `bgra` keeps the filter chain from chroma-subsampling.
            & $Encoder.Path -hide_banner -loglevel error -y -i $Source `
                -vf "scale=${Size}:${Size}:flags=lanczos" -frames:v 1 `
                -c:v libwebp -lossless 0 -preset icon -quality $Quality -pix_fmt bgra `
                $Destination
            $exit = $LASTEXITCODE
        }
    }

    if ($exit -ne 0) {
        throw "webp encode failed with $($Encoder.Kind) (exit $exit): $Destination"
    }
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        throw "no output written: $Destination"
    }
}

$Encoder = Get-WebpEncoder
Write-Host "brand mark: $Source"
Write-Host "webp encoder: $($Encoder.Kind)"

$Scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("maju-icons-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $Scratch | Out-Null

try {
    New-ResizedPng -ImagePath $Source -Size $ExpoIconSize -Destination $ExpoIcon
    Write-Host 'wrote apps/mobile/assets/icon.png (1024x1024)'

    for ($i = 0; $i -lt $Densities.Count; $i++) {
        $density = $Densities[$i]
        $dir = Join-Path $Res "mipmap-$density"
        New-Item -ItemType Directory -Force -Path $dir | Out-Null

        Write-WebpIcon -Size $LegacySizes[$i] -Destination (Join-Path $dir 'ic_launcher.webp')
        Write-WebpIcon -Size $ForegroundSizes[$i] -Destination (Join-Path $dir 'ic_launcher_foreground.webp')

        Remove-Item -Force -ErrorAction SilentlyContinue `
            (Join-Path $dir 'ic_launcher_round.webp'), (Join-Path $dir 'ic_launcher_background.webp')
        Write-Host "wrote res/mipmap-$density/ic_launcher.webp + ic_launcher_foreground.webp"
    }
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Scratch
}

Write-Host 'done — rebuild the mobile app (npx expo run:android) to pick it up'
