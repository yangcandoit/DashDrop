param(
  [Parameter(Mandatory = $true)]
  [string]$SnapshotFile,
  [Parameter(Mandatory = $true)]
  [string]$AdvertisementFile
)

$ErrorActionPreference = "Stop"

function Get-BluetoothHardwarePresent {
  try {
    $devices = Get-PnpDevice -Class Bluetooth -PresentOnly -ErrorAction SilentlyContinue
    return $null -ne $devices -and @($devices).Count -gt 0
  } catch {
    return $false
  }
}

function Get-AdvertisementRequest {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) {
    return $null
  }

  try {
    $raw = Get-Content -LiteralPath $Path -Raw -ErrorAction Stop
    if ([string]::IsNullOrWhiteSpace($raw)) {
      return $null
    }
    return $raw | ConvertFrom-Json -ErrorAction Stop
  } catch {
    return $null
  }
}

function Write-BridgeSnapshot {
  param(
    [string]$Path,
    [bool]$HardwarePresent,
    [string]$AdvertisedRollingIdentifier
  )

  $snapshot = [ordered]@{
    permission_state = "not_required"
    scanner_state = if ($HardwarePresent) { "winrt_scaffold_ready" } else { "hardware_unavailable" }
    advertiser_state = if ($AdvertisedRollingIdentifier) {
      "advertisement_request_seen"
    } elseif ($HardwarePresent) {
      "winrt_scaffold_idle"
    } else {
      "hardware_unavailable"
    }
    advertised_rolling_identifier = $AdvertisedRollingIdentifier
    capsules = @()
  }

  $parent = Split-Path -Parent $Path
  if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
  }

  $tempFile = "$Path.tmp"
  $json = $snapshot | ConvertTo-Json -Depth 6
  Set-Content -LiteralPath $tempFile -Value $json -Encoding UTF8
  Move-Item -LiteralPath $tempFile -Destination $Path -Force
}

while ($true) {
  $hardwarePresent = Get-BluetoothHardwarePresent
  $advertisement = Get-AdvertisementRequest -Path $AdvertisementFile
  $advertisedRollingIdentifier = $null
  if ($null -ne $advertisement -and $null -ne $advertisement.capsule) {
    $advertisedRollingIdentifier = $advertisement.capsule.rolling_identifier
  }

  Write-BridgeSnapshot -Path $SnapshotFile -HardwarePresent $hardwarePresent -AdvertisedRollingIdentifier $advertisedRollingIdentifier
  Start-Sleep -Seconds 5
}
