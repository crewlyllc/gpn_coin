#Requires -Version 5.1
<#
.SYNOPSIS
  Remove the GPN node autostart on Windows (one command).

.DESCRIPTION
  A process that starts itself at logon has to be removable in one step. This
  is a thin, discoverable wrapper around `install.ps1 -Uninstall`: it stops and
  deletes the GPN-Node task, deletes GPN-Ollama (only if the installer created
  it), and leaves the chain DB, the wallet and the binaries alone.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File uninstall.ps1
#>
$ErrorActionPreference = 'Continue'
$installer = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'install.ps1'
if (-not (Test-Path $installer)) {
  Write-Host " !  install.ps1 not found next to uninstall.ps1 ($installer)" -ForegroundColor Yellow
  exit 1
}
& powershell -NoProfile -ExecutionPolicy Bypass -File $installer -Uninstall @args
exit $LASTEXITCODE
