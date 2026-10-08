<#
.SYNOPSIS
  Run iphone-use natively on Windows: start the device runner on a USB iPhone,
  relay its ports and serve the agent API, MCP and web page on this PC.

.DESCRIPTION
  On a Mac, setup-wda.sh does this with xcodebuild and a LaunchAgent. Windows
  has neither, so this script uses:

    - Apple Mobile Device Service (installed with "Apple Devices" or iTunes)
      as usbmuxd, at 127.0.0.1:27015;
    - go-ios (ios.exe, https://github.com/danielpaulus/go-ios) to mount the
      Developer Disk Image, open the iOS 17+ tunnel and start the runner's
      XCTest (RunnerTests/testServe);
    - iphone-use.exe relay for the runner's ports 8100 (control) and 9100
      (video), then iphone-use.exe serve.

  The runner app must already be installed and signed on the phone; see
  docs/windows.md. Stop everything with Ctrl+C.

.EXAMPLE
  .\iphone-use.ps1
  .\iphone-use.ps1 -Udid 00008110-0002346211A0401E -BundleId com.example.runner.xctrunner
#>
[CmdletBinding()]
param(
    # The iPhone's UDID. Default: the only phone go-ios lists.
    [string]$Udid,
    # The installed runner's bundle id (...xctrunner). Default: found on the phone.
    [string]$BundleId,
    # go-ios binary. Default: ios.exe next to this script, then PATH.
    [string]$Ios,
    # iphone-use binary. Default: iphone-use.exe next to this script, then PATH.
    [string]$IphoneUse,
    # Daemon listen address/port. 0.0.0.0 exposes it to the LAN (needs -Password).
    [string]$ListenHost = "127.0.0.1",
    [int]$Port = 44321,
    # Password for the web page; required with a non-loopback -ListenHost.
    [string]$Password,
    # Skip the iOS 17+ tunnel (already running elsewhere).
    [switch]$NoTunnel
)

$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$stateDir = Join-Path $env:USERPROFILE ".iphone-use"
$logDir = Join-Path $stateDir "logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

function Find-Tool([string]$given, [string]$name) {
    if ($given) { return (Resolve-Path $given).Path }
    $local = Join-Path $here "$name.exe"
    if (Test-Path $local) { return $local }
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw "$name.exe not found next to this script or on PATH"
}

function Test-Port([string]$address, [int]$port) {
    $client = New-Object Net.Sockets.TcpClient
    try {
        $async = $client.BeginConnect($address, $port, $null, $null)
        return ($async.AsyncWaitHandle.WaitOne(500) -and $client.Connected)
    } catch { return $false } finally { $client.Close() }
}

function Start-Background([string]$name, [string]$file, [string[]]$arguments) {
    $log = Join-Path $logDir "$name.log"
    $proc = Start-Process -FilePath $file -ArgumentList $arguments -NoNewWindow -PassThru `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    Write-Host "  started $name (pid $($proc.Id)), log: $log"
    return $proc
}

$Ios = Find-Tool $Ios "ios"
$IphoneUse = Find-Tool $IphoneUse "iphone-use"

# -- 1. usbmuxd --------------------------------------------------------------
if (-not (Test-Port "127.0.0.1" 27015)) {
    throw "Apple Mobile Device Service is not answering on 127.0.0.1:27015. " +
          "Install 'Apple Devices' from the Microsoft Store (or iTunes), plug the iPhone in and tap Trust."
}

# Leftovers from an earlier run (a closed window can orphan them) would answer
# instead of this run's runner and keep `serve` from binding its port.
foreach ($busy in @(8100, 9100, $Port)) {
    if (Test-Port "127.0.0.1" $busy) {
        throw "Port $busy is already in use, probably by an earlier run. Stop it first: " +
              "Get-Process iphone-use, ios -ErrorAction SilentlyContinue | Stop-Process"
    }
}

# -- 2. which phone ----------------------------------------------------------
if (-not $Udid) {
    $listed = (& $Ios list | ConvertFrom-Json).deviceList
    if (-not $listed) { throw "No iPhone attached. Plug it in, unlock it and tap Trust." }
    if (@($listed).Count -gt 1) { throw "Several iPhones attached ($($listed -join ', ')); pass -Udid." }
    $Udid = @($listed)[0]
}
$info = & $IphoneUse device info --udid $Udid | ConvertFrom-Json
if (-not $info.ok) { throw "Cannot read the iPhone over usbmuxd: $($info.error)" }
Write-Host "iPhone: $($info.name), iOS $($info.product_version) ($Udid)"
$iosMajor = [int]($info.product_version -split '\.')[0]

$children = New-Object System.Collections.ArrayList
try {
    # -- 3. iOS 17+: the tunnel testmanagerd is reached through --------------
    if ($iosMajor -ge 17 -and -not $NoTunnel) {
        # go-ios's tunnel agent answers on --tunnel-info-port, 28100 by default.
        if (Test-Port "127.0.0.1" 28100) {
            Write-Host "go-ios tunnel agent already running"
        } else {
            [void]$children.Add((Start-Background "tunnel" $Ios @("tunnel", "start", "--userspace")))
            Start-Sleep -Seconds 3
        }
    }

    # -- 4. Developer Disk Image ---------------------------------------------
    & $Ios image auto "--udid=$Udid" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "go-ios could not mount the Developer Disk Image (is Developer Mode on?)" }

    # -- 5. the runner -------------------------------------------------------
    if (-not $BundleId) {
        $apps = & $Ios apps --list "--udid=$Udid"
        # `apps --list` prints "<bundle id> <name> <version>". Sideloadly may
        # rename the bundle id, so match the app name (iPhoneUse-Runner) too.
        $BundleId = ($apps | Where-Object { $_ -match "xctrunner" -and $_ -match "iphone-?use" } |
            ForEach-Object { ($_ -split '\s+')[0] } | Select-Object -First 1)
        if (-not $BundleId) {
            throw "The iPhoneUse-Runner app is not installed on the phone. See docs/windows.md (install the runner IPA)."
        }
    }
    Write-Host "runner: $BundleId"
    $runnerProc = Start-Background "runner" $Ios @(
        "runtest", "--bundle-id=$BundleId", "--test-runner-bundle-id=$BundleId",
        "--xctest-config=iPhoneUse.xctest", "--test-to-run=RunnerTests/testServe", "--udid=$Udid")
    [void]$children.Add($runnerProc)

    # -- 6. relays to the runner's ports -------------------------------------
    [void]$children.Add((Start-Background "relay-8100" $IphoneUse @(
        "relay", "--udid", $Udid, "--listen", "127.0.0.1:8100", "--device-port", "8100")))
    [void]$children.Add((Start-Background "relay-9100" $IphoneUse @(
        "relay", "--udid", $Udid, "--listen", "127.0.0.1:9100", "--device-port", "9100")))

    Write-Host "waiting for the runner (unlock the phone if it is locked)..."
    $deadline = (Get-Date).AddSeconds(90)
    $up = $false
    while ((Get-Date) -lt $deadline) {
        if ($runnerProc.HasExited) {
            $tail = (Get-Content (Join-Path $logDir "runner.log.err") -Tail 15 -ErrorAction SilentlyContinue) -join "`n"
            throw "The runner (ios runtest) exited with code $($runnerProc.ExitCode):`n$tail"
        }
        try {
            Invoke-RestMethod -Uri "http://127.0.0.1:8100/status" -TimeoutSec 3 | Out-Null
            $up = $true
            break
        } catch { Start-Sleep -Seconds 2 }
    }
    if (-not $up) { throw "The runner did not answer within 90 s; see $logDir\runner.log" }
    Write-Host "runner is up"

    # -- 7. the daemon -------------------------------------------------------
    $tokenFile = Join-Path $stateDir "agent-token"
    if (-not (Test-Path $tokenFile)) {
        $bytes = New-Object byte[] 24
        [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
        [IO.File]::WriteAllText($tokenFile, ([BitConverter]::ToString($bytes) -replace '-', '').ToLower())
    }
    $token = (Get-Content $tokenFile -Raw).Trim()

    $env:PHONE_REMOTE_UDID = $Udid
    $env:PHONE_REMOTE_WDA_MANAGED = "0"
    $env:PHONE_REMOTE_HOST = $ListenHost
    $env:PHONE_REMOTE_PORT = "$Port"
    $env:PHONE_REMOTE_AGENT_TOKEN = $token
    if ($Password) { $env:PHONE_REMOTE_PASSWORD = $Password }

    $mcp = Join-Path (Split-Path -Parent $IphoneUse) "iphone-use-mcp.exe"
    Write-Host ""
    Write-Host "Control page:  http://127.0.0.1:$Port/phone"
    Write-Host "Agent API:     http://127.0.0.1:$Port/agent/*  (Authorization: Bearer <token in $tokenFile>)"
    Write-Host "MCP server:    $mcp"
    Write-Host "               env PHONE_REMOTE_URL=http://127.0.0.1:$Port PHONE_REMOTE_TOKEN=<token>"
    Write-Host "Ctrl+C stops the daemon, the relays and the runner."
    Write-Host ""
    & $IphoneUse serve
} finally {
    foreach ($proc in $children) {
        if ($proc -and -not $proc.HasExited) {
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
}
