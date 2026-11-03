# Prefer an installed toolchain; support the isolated tooling used in this workspace.
$taskRoot = Split-Path -Parent $PSScriptRoot
if (Test-Path (Join-Path $taskRoot '.tools\cargo\bin\cargo.exe')) {
    $env:CARGO_HOME = Join-Path $taskRoot '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $taskRoot '.tools\rustup'
    $env:PATH = "$(Join-Path $taskRoot '.tools\cargo\bin');$(Join-Path $taskRoot '.tools\w64devkit\bin');$env:PATH"
}
& cargo @args
exit $LASTEXITCODE
