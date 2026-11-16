$installDir = Join-Path $env:LOCALAPPDATA "Programs\CBXShell"
$relDir = Join-Path $PSScriptRoot "target\release"

Stop-Process -Name "cbx-manager", "cbx-cli", "prevhost" -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 600

# Rename in-use dll so it can be overwritten cleanly without locking
if (Test-Path (Join-Path $installDir "cbxshell.dll.old")) {
    Remove-Item -Force (Join-Path $installDir "cbxshell.dll.old") -ErrorAction SilentlyContinue
}
Move-Item -Force (Join-Path $installDir "cbxshell.dll") (Join-Path $installDir "cbxshell.dll.old") -ErrorAction SilentlyContinue

Copy-Item -Force (Join-Path $relDir "cbxshell.dll") (Join-Path $installDir "cbxshell.dll")
Copy-Item -Force (Join-Path $relDir "cbx-manager.exe") (Join-Path $installDir "cbx-manager.exe")
Copy-Item -Force (Join-Path $relDir "cbx-cli.exe") (Join-Path $installDir "cbx-cli.exe")

$localeSrc = Join-Path $PSScriptRoot "crates\cbx-manager\locales"
$localeDest = Join-Path $installDir "locales"
if (!(Test-Path $localeDest)) { New-Item -ItemType Directory -Path $localeDest -Force }
Copy-Item -Force (Join-Path $localeSrc "*.json") $localeDest

$dll = Join-Path $installDir "cbxshell.dll"
Start-Process regsvr32.exe -ArgumentList "/s `"$dll`"" -Wait

Write-Host "DEPLOY COMPLETED SUCCESSFULLY!"
