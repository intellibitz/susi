[CmdletBinding()]
param(
    [ValidateSet('Install')][string]$Action = 'Install',
    [ValidateSet('dev','production')][string]$Channel = 'production',
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'SusiLocal'),
    [string]$ShortcutDirectory,
    [string]$Candidate
)
$ErrorActionPreference = 'Stop'
$Root = [IO.Path]::GetFullPath($Root)
$channelRoot = Join-Path $Root $Channel
$state = if ($Action -eq 'Install' -and $Candidate) {
    @{ current=$Candidate; sha256=(Get-FileHash -LiteralPath $Candidate -Algorithm SHA256).Hash.ToLowerInvariant() }
} else { Get-Content -LiteralPath (Join-Path $channelRoot 'channel.json') -Raw | ConvertFrom-Json }
if ((Get-FileHash -LiteralPath $state.current -Algorithm SHA256).Hash.ToLowerInvariant() -ne $state.sha256) {
    throw 'The installed local executable changed; rebuild or roll back before launching.'
}
if ($Action -eq 'Install') {
    if (!$ShortcutDirectory) { $ShortcutDirectory = [Environment]::GetFolderPath('Programs') }
    New-Item -ItemType Directory -Path $ShortcutDirectory -Force | Out-Null
    $name = if ($Channel -eq 'production') { 'Susi (local production)' } else { 'Susi (development)' }
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut((Join-Path $ShortcutDirectory "$name.lnk"))
    $shortcut.TargetPath = $state.current
    $shortcut.Arguments = 'shell'
    $workspace = Join-Path $channelRoot 'workspace'
    New-Item -ItemType Directory -Path $workspace -Force | Out-Null
    $shortcut.WorkingDirectory = $workspace
    $shortcut.Description = $name
    $shortcut.IconLocation = $state.current
    $shortcut.Save()
    Write-Output "Installed Start Menu app: $name"
    return
}
