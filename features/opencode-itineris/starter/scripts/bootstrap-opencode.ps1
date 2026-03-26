param(
    [Parameter(Mandatory = $true)]
    [string]$TargetRepo,

    [ValidateSet('none', 'dotnet-only', 'frontend-heavy', 'platform-heavy')]
    [string]$Variant = 'none',

    [switch]$IncludeProviderNotes
)

$ErrorActionPreference = 'Stop'

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$starterRoot = Split-Path -Parent $scriptRoot
$targetPath = [System.IO.Path]::GetFullPath($TargetRepo)

if (-not (Test-Path -Path $targetPath)) {
    throw "Target repository does not exist: $targetPath"
}

Copy-Item -Path (Join-Path $starterRoot '.opencode') -Destination $targetPath -Recurse -Force
Copy-Item -Path (Join-Path $starterRoot 'opencode.json') -Destination (Join-Path $targetPath 'opencode.json') -Force

if ($Variant -ne 'none') {
    $variantConfig = Join-Path $starterRoot (Join-Path 'variants' (Join-Path $Variant 'opencode.json'))
    $variantCopy = Join-Path $targetPath ("opencode.$Variant.overlay.json")
    Copy-Item -Path $variantConfig -Destination $variantCopy -Force
    Write-Host "Copied overlay config to $variantCopy"
    Write-Host 'Merge the overlay into opencode.json using your preferred JSON merge process.'
}

if ($IncludeProviderNotes) {
    Copy-Item -Path (Join-Path $starterRoot 'PROVIDER_SETUP.md') -Destination (Join-Path $targetPath 'PROVIDER_SETUP.md') -Force
}

Write-Host "Installed .opencode assets into $targetPath"
Write-Host 'Next steps:'
Write-Host '1. Review opencode.json bash allowlists for the target repo.'
Write-Host '2. Review PROVIDER_SETUP.md if your team uses custom provider catalogs.'
Write-Host '3. Start with /plan-slice in OpenCode to validate discovery and routing.'