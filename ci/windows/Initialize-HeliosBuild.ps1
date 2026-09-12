Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function ConvertTo-WindowsKitVersion([Parameter(Mandatory)][string]$Value) {
    $version = [version]"0.0"
    if ([version]::TryParse($Value, [ref]$version)) { return $version }
    return [version]"0.0"
}

function Import-VisualStudioEnvironment(
    [ValidateSet("x64", "x86")][string]$Architecture = "x64"
) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw "vswhere.exe was not found; Visual Studio 2022 with C++ tools is required."
    }

    $installation = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath).Trim()
    if (-not $installation) {
        throw "Visual Studio 2022 with the x86/x64 C++ toolchain was not found."
    }

    $devCmd = Join-Path $installation "Common7\Tools\VsDevCmd.bat"
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $env:SystemRoot "System32\cmd.exe"
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $clean = ""
    if ($env:__VSCMD_PREINIT_PATH) {
        # Repeated imports otherwise accumulate VS paths until cmd's 8191-byte
        # line limit is reached. Reset in the child before even -clean_env runs;
        # its batch files also expand PATH. VS owns the rest of its cleanup.
        $baseline = $env:__VSCMD_PREINIT_PATH
        $additions = @()
        if ($env:HELIOS_VS_IMPORTED_PATH) {
            # Preserve paths added by our caller after the previous import
            # (LLVM/WDK/MSYS2, for example). This marker travels to Cargo's
            # child PowerShell process, unlike a script-scoped cache.
            $imported = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
            foreach ($entry in $env:HELIOS_VS_IMPORTED_PATH.Split(';')) { [void]$imported.Add($entry) }
            $additions = @($env:PATH.Split(';') | Where-Object { $_ -and -not $imported.Contains($_) } | Select-Object -Unique)
        } else {
            # A caller may enter from a Developer shell we did not initialize.
            # Preserve its extra tools while discarding VS-owned paths (which
            # include the old architecture's compiler). The explicit LLVM
            # selection may itself live inside the VS installation.
            $vsRoots = @($env:VSINSTALLDIR, $installation) | Where-Object { $_ } | ForEach-Object { $_.TrimEnd('\') + '\' }
            $additions = @($env:PATH.Split(';') | Where-Object {
                $entry = $_
                $entry -and -not @($vsRoots | Where-Object { $entry.StartsWith($_, [StringComparison]::OrdinalIgnoreCase) }).Count
            })
            if ($env:LIBCLANG_PATH) { $additions = @($env:LIBCLANG_PATH) + $additions }
        }
        if ($additions.Count -gt 0) {
            $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
            $baseline = (@($additions + $baseline.Split(';') | Where-Object { $_ -and $seen.Add($_) }) -join ';')
        }
        $start.Environment["PATH"] = $baseline
        $start.Environment["__VSCMD_PREINIT_PATH"] = $baseline
        foreach ($name in @("INCLUDE", "LIB", "LIBPATH", "EXTERNAL_INCLUDE")) {
            $previous = [Environment]::GetEnvironmentVariable("__VSCMD_PREINIT_$name")
            if ($previous) { $start.Environment[$name] = $previous }
            else { [void]$start.Environment.Remove($name) }
        }
        $clean = "call `"$devCmd`" -no_logo -clean_env && "
    }
    $start.Arguments = "/d /s /c `"${clean}call `"$devCmd`" -no_logo -arch=$Architecture -host_arch=x64 && set`""
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { throw "Could not start VsDevCmd.bat." }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) {
            throw "VsDevCmd.bat failed with exit code $($process.ExitCode): $($stderr.Result)"
        }
        $environment = @{}
        foreach ($line in ($stdout.Result -split "`r?`n")) {
            $parts = $line -split "=", 2
            if ($parts.Count -eq 2 -and $parts[0]) { $environment[$parts[0]] = $parts[1] }
        }
    } finally {
        $process.Dispose()
    }
    # Mirror removals too: assigning only returned variables would retain stale
    # architecture-specific state that -clean_env removed in the child.
    foreach ($entry in @(Get-ChildItem Env:)) {
        if (-not $environment.ContainsKey($entry.Name)) { Remove-Item -LiteralPath "Env:$($entry.Name)" }
    }
    foreach ($name in $environment.Keys) {
        Set-Item -LiteralPath "Env:$name" -Value $environment[$name]
    }
    $env:HELIOS_VS_IMPORTED_PATH = $env:PATH
}

function Find-WindowsKitTool([Parameter(Mandatory)][string]$Name) {
    $kitsBin = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    if (-not (Test-Path -LiteralPath $kitsBin -PathType Container)) {
        throw "Windows Kits bin directory was not found at $kitsBin."
    }
    $tools = @(Get-ChildItem -LiteralPath $kitsBin -Filter $Name -File -Recurse |
        Where-Object { $_.Directory.Name -in @("x64", "x86") } |
        Sort-Object { ConvertTo-WindowsKitVersion $_.Directory.Parent.Name } -Descending)
    $tool = $tools | Where-Object { $_.Directory.Name -eq "x64" } | Select-Object -First 1
    if (-not $tool) { $tool = $tools | Select-Object -First 1 }
    if (-not $tool) {
        throw "$Name was not found below $kitsBin. Install the Windows 11 WDK."
    }
    return $tool.FullName
}

function Find-WindowsKitInclude {
    $includeRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\Include"
    $candidate = Get-ChildItem -LiteralPath $includeRoot -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "km\ntddk.h") } |
        Sort-Object { ConvertTo-WindowsKitVersion $_.Name } -Descending |
        Select-Object -First 1
    if (-not $candidate) {
        throw "A Windows 11 WDK include tree was not found below $includeRoot."
    }
    return $candidate.FullName
}

function Assert-Command([Parameter(Mandatory)][string]$Name) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if (-not $command) { throw "Required command is missing from PATH: $Name" }
    return $command.Source
}
