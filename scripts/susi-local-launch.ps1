[CmdletBinding()]
param(
    [ValidateSet('Install','Run')][string]$Action = 'Run',
    [ValidateSet('dev','production')][string]$Channel = 'production',
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'SusiLocal'),
    [string]$ShortcutDirectory,
    [string]$CanonicalHome,
    [string]$Candidate,
    [string[]]$SusiArgs = @('shell')
)
$ErrorActionPreference = 'Stop'
$Root = [IO.Path]::GetFullPath($Root)
$channelRoot = Join-Path $Root $Channel
$channelHome = Join-Path $channelRoot 'home'
$state = if ($Action -eq 'Install' -and $Candidate) {
    @{ current=$Candidate; sha256=(Get-FileHash -LiteralPath $Candidate -Algorithm SHA256).Hash.ToLowerInvariant() }
} else { Get-Content -LiteralPath (Join-Path $channelRoot 'channel.json') -Raw | ConvertFrom-Json }
if ((Get-FileHash -LiteralPath $state.current -Algorithm SHA256).Hash.ToLowerInvariant() -ne $state.sha256) {
    throw 'The installed local executable changed; rebuild or roll back before launching.'
}
if ($Action -eq 'Install') {
    $launcher = Join-Path $channelRoot 'launcher.ps1'
    if ([IO.Path]::GetFullPath($PSCommandPath) -ne $launcher) {
        Copy-Item -LiteralPath $PSCommandPath -Destination "$launcher.$PID.new" -Force
        Move-Item -LiteralPath "$launcher.$PID.new" -Destination $launcher -Force
    }
    if (!$ShortcutDirectory) { $ShortcutDirectory = [Environment]::GetFolderPath('Programs') }
    New-Item -ItemType Directory -Path $ShortcutDirectory -Force | Out-Null
    $name = if ($Channel -eq 'production') { 'Susi (local production)' } else { 'Susi (development)' }
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut((Join-Path $ShortcutDirectory "$name.lnk"))
    $shortcut.TargetPath = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
    $shortcut.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$launcher`" -Channel $Channel -Root `"$Root`""
    if ($CanonicalHome) { $shortcut.Arguments += " -CanonicalHome `"$CanonicalHome`"" }
    $shortcut.WorkingDirectory = [Environment]::GetFolderPath('UserProfile')
    $shortcut.Description = $name
    $shortcut.IconLocation = $state.current
    $shortcut.Save()
    Write-Output "Installed Start Menu app: $name"
    return
}

if (!$CanonicalHome) {
    $CanonicalHome = if ($env:SUSI_HOME -and [IO.Path]::GetFullPath($env:SUSI_HOME) -ne $channelHome) {
        $env:SUSI_HOME
    } else { Join-Path $env:USERPROFILE '.susi' }
}
$CanonicalHome = [IO.Path]::GetFullPath($CanonicalHome)
New-Item -ItemType Directory -Path $channelHome -Force | Out-Null

# Add a read-only discovery location, never a writable junction into the
# production cache. New downloads and all mutable state stay in this channel.
$models = Join-Path $CanonicalHome 'models'
if (Test-Path -LiteralPath $models -PathType Container) {
    $configPath = Join-Path $channelHome 'config.json'
    $config = if (Test-Path -LiteralPath $configPath) { Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json } else { New-Object PSObject }
    $paths = @($config.local_scan_paths | Where-Object { $_ })
    if ($models -notin $paths) {
        $config | Add-Member -MemberType NoteProperty -Name local_scan_paths -Value @($paths + $models) -Force
        $temporary = "$configPath.$PID.new"
        [IO.File]::WriteAllText($temporary, ($config | ConvertTo-Json -Depth 100), (New-Object Text.UTF8Encoding($false)))
        Move-Item -LiteralPath $temporary -Destination $configPath -Force
    }
}

$saved = @{}
try {
    # Reuse stored provider connections without copying cloud.env or importing
    # arbitrary command, PATH, or SUSI_HOME settings. Existing process values win.
    $cloud = Join-Path $CanonicalHome 'cloud.env'
    if (Test-Path -LiteralPath $cloud -PathType Leaf) {
        foreach ($line in [IO.File]::ReadAllLines($cloud)) {
            $entry = $line.Trim() -replace '^export\s+', ''
            if ($entry -notmatch '^([A-Za-z][A-Za-z0-9_]*_(?:API_KEY|API_BASE|BASE_URL|MODEL|API_VERSION))\s*=(.*)$') { continue }
            $key = $Matches[1]
            $value = $Matches[2].Trim()
            if ($value.Length -ge 2 -and (($value.StartsWith('"') -and $value.EndsWith('"')) -or ($value.StartsWith("'") -and $value.EndsWith("'")))) { $value = $value.Substring(1, $value.Length - 2) }
            if (!$value -or [Environment]::GetEnvironmentVariable($key, 'Process')) { continue }
            $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            [Environment]::SetEnvironmentVariable($key, $value, 'Process')
        }
    }
    foreach ($key in @('SUSI_HOME','SUSI_PORT_OFFSET')) { $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process') }
    $env:SUSI_HOME = $channelHome
    $env:SUSI_PORT_OFFSET = if ($Channel -eq 'dev') { '200' } else { '300' }
    & $state.current @SusiArgs
    $result = $LASTEXITCODE
} finally {
    foreach ($key in $saved.Keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process') }
}
if ($result -ne 0) { throw "Local $Channel Susi exited with code $result." }
