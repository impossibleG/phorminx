[CmdletBinding()]
param(
    [switch] $Vulkan
)

$ErrorActionPreference = 'Stop'

function Set-EnvironmentFromVisualStudio {
    $vsWherePath = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vsWherePath)) {
        throw 'Visual Studio Build Tools were not found. Install the Desktop development with C++ workload.'
    }

    $installationPath = & $vsWherePath -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $installationPath) {
        throw 'The Visual Studio C++ toolchain was not found.'
    }

    $devCommandPath = Join-Path $installationPath 'Common7\Tools\VsDevCmd.bat'
    $environmentLines = & cmd.exe /d /s /c "call `"$devCommandPath`" -arch=x64 -no_logo && set"
    foreach ($line in $environmentLines) {
        if ($line -match '^([^=]+)=(.*)$') {
            [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
        }
    }
}
function Find-FirstFile([string[]] $Candidates) {
    foreach ($candidate in $Candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Get-Item -LiteralPath $candidate).FullName
        }
    }

    return $null
}

Set-EnvironmentFromVisualStudio

$cmakeCommand = Get-Command cmake -ErrorAction SilentlyContinue
$cmakePath = Find-FirstFile @(
    $cmakeCommand.Source,
    (Join-Path $env:ProgramFiles 'CMake\bin\cmake.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe')
)
if (-not $cmakePath) {
    throw 'CMake was not found.'
}
$env:CMAKE = $cmakePath

$libClangPath = Find-FirstFile @(
    (Join-Path $env:USERPROFILE 'Tools\LLVM\bin\libclang.dll'),
    (Join-Path $env:ProgramFiles 'LLVM\bin\libclang.dll')
)
if (-not $libClangPath) {
    throw 'libclang.dll was not found. Install LLVM or place it under ~/Tools/LLVM.'
}
$env:LIBCLANG_PATH = Split-Path -Parent $libClangPath

if ($Vulkan) {
    $vulkanRoots = @()
    if ($env:VULKAN_SDK) {
        $vulkanRoots += $env:VULKAN_SDK
    }
    $vulkanRoots += Get-ChildItem -Path (Join-Path $env:USERPROFILE 'Tools') -Directory -Filter 'VulkanSDK-*' -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        Select-Object -ExpandProperty FullName
    $vulkanRoots += Get-ChildItem -Path 'C:\VulkanSDK' -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        Select-Object -ExpandProperty FullName

    $vulkanRoot = $vulkanRoots |
        Where-Object { Test-Path -LiteralPath (Join-Path $_ 'Lib\vulkan-1.lib') } |
        Select-Object -First 1
    if (-not $vulkanRoot) {
        throw 'The Vulkan SDK was not found.'
    }
    $env:VULKAN_SDK = $vulkanRoot

    $ninjaCommand = Get-Command ninja -ErrorAction SilentlyContinue
    $ninjaCandidates = @($ninjaCommand.Source)
    $ninjaCandidates += Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Packages') -Filter ninja.exe -Recurse -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty FullName
    $ninjaPath = Find-FirstFile $ninjaCandidates
    if (-not $ninjaPath) {
        throw 'Ninja was not found.'
    }

    $env:CMAKE_GENERATOR = 'Ninja'
    $env:CMAKE_MAKE_PROGRAM = $ninjaPath
    $env:PATH = "$(Split-Path -Parent $ninjaPath);$(Join-Path $vulkanRoot 'Bin');$env:PATH"
}

Write-Host "Phorminx developer environment ready. Vulkan: $Vulkan"
Write-Host "CMake: $env:CMAKE"
Write-Host "libclang: $env:LIBCLANG_PATH"
if ($Vulkan) {
    Write-Host "Vulkan SDK: $env:VULKAN_SDK"
    Write-Host "Ninja: $env:CMAKE_MAKE_PROGRAM"
}
