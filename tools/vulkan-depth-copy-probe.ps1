# Isolated Vulkan correctness diagnostic. A completed mismatch is not a pass.
[CmdletBinding()]
param(
    [ValidateSet('Build','Schedule','Run')][string]$Mode='Build',
    [string]$BuildDir='C:\ProgramData\Helios\depth-copy-probe',
    [string]$ArchiveRoot='Z:\tmp\fl12-d32-copy-20260908',
    [string]$InteractiveUser='Rupansh',
    [string]$ExpectedIcdSHA256=''
)
$executedSource=$MyInvocation.MyCommand.ScriptBlock.Ast.Extent.Text
$ErrorActionPreference='Stop'
if($BuildDir -notmatch '^[Cc]:\\') {throw 'BuildDir must be on local C: disk.'}
$BuildDir=[IO.Path]::GetFullPath($BuildDir).TrimEnd('\')
$runner=Join-Path $BuildDir 'vulkan-depth-copy-probe.ps1'
$exe=Join-Path $BuildDir 'probe.exe'
$receiptPath=Join-Path $BuildDir 'build.json'
function Artifact([string]$path) {
    $file=Get-Item -LiteralPath $path
    [ordered]@{Path=$file.FullName;SHA256=(Get-FileHash -LiteralPath $path).Hash;Length=$file.Length}
}
if($Mode -eq 'Build') {
    $sourceRoot=Split-Path $PSScriptRoot -Parent
    $inputs=@('tools\vulkan_depth_copy_probe.c','tools\vulkan_depth_copy_probe\fullscreen.vert',
        'tools\vulkan_depth_copy_probe\write.frag','tools\vulkan_depth_copy_probe\read.comp',
        'tools\vulkan_depth_copy_probe\build_shaders.py')
    New-Item -ItemType Directory -Force -Path $BuildDir,"$BuildDir\generated" | Out-Null
    Remove-Item -LiteralPath $receiptPath -ErrorAction SilentlyContinue
    foreach($relative in $inputs) {
        $dest=Join-Path "$BuildDir\src" $relative
        New-Item -ItemType Directory -Force -Path (Split-Path $dest -Parent) | Out-Null
        [IO.File]::WriteAllBytes($dest,[IO.File]::ReadAllBytes((Join-Path $sourceRoot $relative)))
    }
    [IO.File]::WriteAllText($runner,$executedSource)
    $vswhere="${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs=(& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
    if(!$vs -or !$env:VULKAN_SDK) {throw 'MSVC and Vulkan SDK required.'}
    & python.exe "$BuildDir\src\tools\vulkan_depth_copy_probe\build_shaders.py" "$BuildDir\generated" --glslang "$env:VULKAN_SDK\Bin\glslangValidator.exe" > "$BuildDir\shaders.log" 2>&1
    if($LASTEXITCODE) {throw "Shader build failed: $LASTEXITCODE"}
    $cmd=@"
@echo off
call "$vs\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 exit /b 1
"C:\Program Files\LLVM\bin\clang-cl.exe" /nologo /TC /std:c11 /W4 /WX /clang:-Wno-missing-field-initializers /I"$env:VULKAN_SDK\Include" /I"$BuildDir\generated" "$BuildDir\src\tools\vulkan_depth_copy_probe.c" /Fo"$BuildDir\probe.obj" /Fe"$exe" /link /LIBPATH:"$env:VULKAN_SDK\Lib" vulkan-1.lib
exit /b %errorlevel%
"@
    [IO.File]::WriteAllText("$BuildDir\build.cmd",$cmd)
    & cmd.exe /d /c "`"$BuildDir\build.cmd`" > `"$BuildDir\build.log`" 2>&1"
    if($LASTEXITCODE) {Get-Content "$BuildDir\build.log"; throw "Probe build failed: $LASTEXITCODE"}
    $artifacts=@($inputs | ForEach-Object {Artifact (Join-Path "$BuildDir\src" $_)})
    $artifacts+=@($runner,$exe,"$BuildDir\build.cmd","$BuildDir\build.log","$BuildDir\shaders.log" | ForEach-Object {Artifact $_})
    $artifacts+=@(Get-ChildItem "$BuildDir\generated" -File | ForEach-Object {Artifact $_.FullName})
    $receipt=[ordered]@{UTC=[DateTime]::UtcNow.ToString('o');BuildSucceeded=$true;Artifacts=$artifacts;VisualStudio=$vs;
        VulkanSDK=$env:VULKAN_SDK;Clang=(& 'C:\Program Files\LLVM\bin\clang-cl.exe' --version | Out-String);
        Glslang=(& "$env:VULKAN_SDK\Bin\glslangValidator.exe" --version | Out-String)}
    [IO.File]::WriteAllText($receiptPath,($receipt | ConvertTo-Json -Depth 8))
    Write-Output ($receipt | ConvertTo-Json -Depth 8)
    exit 0
}
if($ExpectedIcdSHA256 -notmatch '\A[0-9a-fA-F]{64}\z') {throw 'Provide exact expected loaded ICD SHA256.'}
$receipt=Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
if(!$receipt.BuildSucceeded -or @($receipt.Artifacts).Count -ne 22) {throw 'Incomplete build receipt.'}
foreach($artifact in $receipt.Artifacts) {
    if((Get-FileHash -LiteralPath $artifact.Path).Hash -ne $artifact.SHA256) {throw "Changed input/artifact: $($artifact.Path)"}
}
if($Mode -eq 'Schedule') {
    $taskName='HeliosDepthCopyProbe'
    $old=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if($old -and $old.State -eq 'Running') {throw 'Depth probe is already running.'}
    $arguments="-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$runner`" -Mode Run -BuildDir `"$BuildDir`" -ArchiveRoot `"$ArchiveRoot`" -ExpectedIcdSHA256 $ExpectedIcdSHA256"
    $action=New-ScheduledTaskAction -Execute powershell.exe -Argument $arguments
    # The loader ignores process-local driver overrides with an elevated token.
    # This diagnostic needs no administrator rights; retain exact ICD hash checks.
    $principal=New-ScheduledTaskPrincipal -UserId $InteractiveUser -LogonType Interactive -RunLevel Limited
    # Never kill a timed-out GPU owner; it retains allocations while draining its fence.
    $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    Write-Output "Started interactive $taskName. Read result.json in the new archive."
    exit 0
}
$session=(Get-Process -Id $PID).SessionId
if(!$session) {throw 'GPU diagnostic requires an interactive scheduled task.'}
if($executedSource.TrimEnd() -ne [IO.File]::ReadAllText($runner).TrimEnd()) {throw 'Runner changed since build.'}
if(Get-Process | Where-Object {$_.ProcessName -match '^3DMark'}) {throw 'Do not overlap this probe with a benchmark.'}
$env:HELIOS_WSI_ASYNC_PRESENT='1'; Remove-Item Env:\HELIOS_RETIRE_FEEDBACK -ErrorAction SilentlyContinue; $env:VK_LAYER_VALIDATE_SYNC='1'
$runName='guest-depth-'+(Get-Date -Format 'yyyyMMdd-HHmmss-fff')+'-'+[Guid]::NewGuid().ToString('N').Substring(0,8)
$runDir=Join-Path $BuildDir $runName
$archive=Join-Path $ArchiveRoot $runName
New-Item -ItemType Directory -Path $runDir | Out-Null
$result=[ordered]@{UTC=[DateTime]::UtcNow.ToString('o');Session=$session;PID=$PID;Completed=$false;Exact=$false;
    Scope='Vulkan depth-copy diagnostic only; no native D3D12 or performance acceptance';Build=$receipt;Error=$null;
    ExpectedIcdSHA256=$ExpectedIcdSHA256;ExitCode=$null;Result=$null;Icd=$null;
    LoaderEnvironment=@{VK_DRIVER_FILES=$env:VK_DRIVER_FILES;VK_ICD_FILENAMES=$env:VK_ICD_FILENAMES}}
$locks=@()
try {
    foreach($artifact in $receipt.Artifacts) {
        $locks+=[IO.File]::Open($artifact.Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        if((Get-FileHash -LiteralPath $artifact.Path).Hash -ne $artifact.SHA256) {throw 'Build changed before execution.'}
    }
    Copy-Item -LiteralPath $exe -Destination "$runDir\probe.exe"
    & cmd.exe /d /c "`"$runDir\probe.exe`" --venus > `"$runDir\probe.log`" 2>&1"
    $result.ExitCode=$LASTEXITCODE
    $lines=@(Get-Content -LiteralPath "$runDir\probe.log")
    $modules=@($lines | Where-Object {$_ -match '^MODULE .*vulkan_virtio.*\.dll$'} | ForEach-Object {$_.Substring(7)} | Select-Object -Unique)
    if($modules.Count -ne 1) {throw 'Probe did not identify exactly one loaded Venus ICD.'}
    $result.Icd=Artifact $modules[0]
    if($result.Icd.SHA256 -ne $ExpectedIcdSHA256) {throw 'Unexpected loaded ICD.'}
    $final=@($lines | Where-Object {$_ -match '^RESULT exit='})
    if($final.Count -ne 1) {throw 'Missing final probe result.'}
    # Get-Content adds provider metadata to strings. Serialize a plain regex
    # value, not that extended object (which can recurse through provider state).
    $result.Result=[regex]::Match($final[0].ToString(),'^RESULT .*$').Value
    $match=[regex]::Match($final[0],'^RESULT exit=([01]) completed_cases=4 checked=576 differences=(\d+) validation_errors=0 timeout=0$')
    if(!$match.Success -or [int]$match.Groups[1].Value -ne $result.ExitCode) {throw 'Incomplete or invalid diagnostic.'}
    $result.Completed=$true
    $result.Exact=$result.ExitCode -eq 0 -and [int]$match.Groups[2].Value -eq 0
} catch {$result.Error=$_.Exception.ToString()}
finally {foreach($lock in $locks) {$lock.Dispose()}}
foreach($name in @('src','generated','build.json','build.cmd','build.log','shaders.log','vulkan-depth-copy-probe.ps1')) {
    Copy-Item -LiteralPath (Join-Path $BuildDir $name) -Destination (Join-Path $runDir $name) -Recurse
}
[IO.File]::WriteAllText("$runDir\result.json",($result | ConvertTo-Json -Depth 8))
New-Item -ItemType Directory -Force -Path $ArchiveRoot | Out-Null
Copy-Item -LiteralPath $runDir -Destination $archive -Recurse
foreach($file in Get-ChildItem -LiteralPath $runDir -Recurse -File) {
    $dest=Join-Path $archive $file.FullName.Substring($runDir.Length+1)
    if((Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath $dest).Hash) {throw "Archive mismatch: $dest"}
}
Write-Output "$archive\result.json"
if(!$result.Completed) {exit 77}
exit $result.ExitCode
