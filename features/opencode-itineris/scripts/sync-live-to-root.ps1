param()

$ErrorActionPreference = 'Stop'

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$featureRoot = Split-Path -Parent $scriptRoot
$repoRoot = Split-Path -Parent (Split-Path -Parent $featureRoot)

Copy-Item -Path (Join-Path $featureRoot '.opencode') -Destination $repoRoot -Recurse -Force
Copy-Item -Path (Join-Path $featureRoot 'opencode.json') -Destination (Join-Path $repoRoot 'opencode.json') -Force

Write-Host 'Synced features/opencode-itineris runtime files to the repository root.'