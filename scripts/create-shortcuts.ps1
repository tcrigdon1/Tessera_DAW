# Creates the Tessera and Update Tessera shortcuts on the desktop and in the
# Start menu. Called by "Create Desktop Shortcut.cmd".
param([Parameter(Mandatory = $true)][string]$Root)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path $Root).Path
$exe = Join-Path $Root 'target\release\tessera.exe'
$icon = Join-Path $Root 'crates\app\assets\tessera.ico'
$updater = Join-Path $Root 'Update Tessera.cmd'
if (-not (Test-Path $exe)) { throw "Tessera is not built yet: $exe" }

$shell = New-Object -ComObject WScript.Shell
foreach ($folder in @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))) {
    $link = $shell.CreateShortcut((Join-Path $folder 'Tessera.lnk'))
    $link.TargetPath = $exe
    # The plug-in bridge is tessera.exe itself, so it is always found.
    $link.WorkingDirectory = Split-Path $exe
    $link.IconLocation = "$icon,0"
    $link.Description = 'Tessera'
    $link.Save()

    $link = $shell.CreateShortcut((Join-Path $folder 'Update Tessera.lnk'))
    $link.TargetPath = $updater
    $link.WorkingDirectory = $Root
    $link.IconLocation = "$icon,0"
    $link.Description = 'Download, build and start the newest Tessera'
    $link.Save()
}
Write-Host 'Created the Tessera and Update Tessera shortcuts on the desktop and in the Start menu.'
