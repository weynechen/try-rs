#!/usr/bin/env pwsh
# Integration test: validate the PowerShell scripts that `try` emits actually
# work in a real PowerShell session. Run from any shell:
#   pwsh -NoProfile -File scripts/ps-integration-test.ps1
$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\try.exe'
if (-not (Test-Path $exe)) { throw "build first: cargo build --release ($exe missing)" }

# Isolate the workspace config so this test never mutates the user's real
# ~/.config/try/workspaces (dirs::home_dir ignores env overrides on Windows,
# so TRY_CONFIG is the only safe isolation mechanism).
$env:TRY_CONFIG = Join-Path ([System.IO.Path]::GetTempPath()) ("try-it-cfg-" + [System.Guid]::NewGuid().ToString('N'))

$failures = 0
function Check($name, $cond) {
    if ($cond) { Write-Host "  ok   $name" }
    else { Write-Host "  FAIL $name" -ForegroundColor Red; $script:failures++ }
}

function Assert-ValidSyntax($name, $code) {
    $errors = $null
    [System.Management.Automation.Language.Parser]::ParseInput($code, [ref]$null, [ref]$errors) | Out-Null
    Check "$name parses without syntax errors" ($errors.Count -eq 0)
    if ($errors.Count -gt 0) { $errors | ForEach-Object { Write-Host "    $_" -ForegroundColor Red } }
}

Write-Host "== init script =="
$initOut = & $exe init --shell powershell 'C:/Users/me/experiments' | Out-String
Assert-ValidSyntax 'init' $initOut

# Evaluate the init script in a child scope and confirm the wrapper + env vars.
Invoke-Expression $initOut
Check 'tr function defined'       ($null -ne (Get-Command tr -ErrorAction SilentlyContinue))
Check 'no reserved try function'  ($initOut -notmatch 'function\s+try\b')
Check 'TRY_PATH env set'          ($env:TRY_PATH -eq 'C:/Users/me/experiments')
Check 'TRY_SHELL env set'         ($env:TRY_SHELL -eq 'powershell')

# Prove a bare `tr` token is parsed as a command invocation, not the reserved
# `try` keyword (the bug we are guarding against).
$kwErrors = $null
[System.Management.Automation.Language.Parser]::ParseInput('tr foo', [ref]$null, [ref]$kwErrors) | Out-Null
Check 'bare `tr` parses as command' ($kwErrors.Count -eq 0)
$tryErrors = $null
[System.Management.Automation.Language.Parser]::ParseInput('try foo', [ref]$null, [ref]$tryErrors) | Out-Null
Check 'bare `try` is a keyword (errors)' ($tryErrors.Count -gt 0)

Write-Host "== clone script =="
$env:TRY_PATH = 'C:/Users/me/experiments'
$cloneOut = & $exe clone https://github.com/user/repo.git | Out-String
Assert-ValidSyntax 'clone' $cloneOut
Check 'clone uses Set-Location'   ($cloneOut -match 'Set-Location -LiteralPath')
Check 'clone uses New-Item'       ($cloneOut -match 'New-Item -ItemType Directory')
Check 'clone forward slashes'     ($cloneOut -notmatch '\\repo-')

Write-Host "== mkdir+cd round-trip (eval a generated Set-Location) =="
# Build a representative MkdirCd/Set sequence by hand using the same shapes the
# binary emits, then prove PowerShell can actually run them against a temp dir.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("try-ps-it-" + [System.Guid]::NewGuid().ToString('N'))
$seq = "New-Item -ItemType Directory -Force -Path '$tmp' | Out-Null; Set-Location -LiteralPath '$tmp'"
Assert-ValidSyntax 'mkdir+cd' $seq
$before = Get-Location
Invoke-Expression $seq
Check 'Set-Location changed dir'  ((Get-Location).Path -eq (Resolve-Path $tmp).Path)
Set-Location $before
Remove-Item -Recurse -Force $tmp

if ($failures -gt 0) { Write-Host "`n$failures check(s) failed" -ForegroundColor Red; exit 1 }
if (Test-Path $env:TRY_CONFIG) { Remove-Item -Force $env:TRY_CONFIG }
Write-Host "`nAll PowerShell integration checks passed" -ForegroundColor Green
