# Negative control matrix for the routing golden set.
#
# ASCII comments only: CLAUDE.md notes that Chinese comments in a UTF-8 script
# get parsed as GBK on a Chinese Windows and break statement boundaries.
#
# Each row edits ONE place in score.rs inside an isolated worktree, runs
# route_golden, and reports whether it went red.
#
# Measured 2026-10-05: 6 red / 3 green. Every green has a measured reason, and
# none of them is a blind spot in the golden set:
#
#   RED   fastest latency weight 0.55 -> 0.05
#   RED   reliable health weight 0.6 -> 0.2
#   RED   capability weight 0.2 -> 0.02 (balanced + custom + default)
#   RED   smartest capability weight 0.6 -> 0.02
#   RED   exact_match bonus 1.25 -> 1.0
#   RED   latency decay constant 2000 -> 200
#   GREEN latency exemption threshold 2000ms -> 200ms
#         Invalid probe, NOT a blind spot: latency_score has two arms, and
#         (2000.0 / ms) for ms <= 2000 is still >= 1.0 and clamps back to 1.0.
#         Changing the threshold alone leaves the function identical. The
#         decay-constant probe above is the one that actually bites.
#   GREEN balanced health weight 0.35 -> 0.36
#         A monotone weight nudge. Candidate scores differ by ~2.4% while the
#         nudge moves them ~0.16%, so no ranking flips. That is correct: the
#         golden guards the RANKING (CLAUDE.md's "sorting stays bit-identical"),
#         not the weight values. A weight change that reorders nothing has not
#         broken the invariant, so green here is the desired outcome.
#   GREEN long-context bonus 0.15 -> 0.30
#         No-op at these data points: intelligence 95 already gives
#         cap = 0.95 + 0.15 + 0.05 = 1.15, clamped to 1.0. Raising the bonus
#         past 0.05 cannot move a candidate that is already at the ceiling.
#
# Run: pwsh -NoProfile -File scripts/route-golden-negative.ps1

$ErrorActionPreference = 'Continue'
$root = 'D:\Java\GitHub\llm-auto\llm-gateway'
$wt   = "$env:TEMP\lgw-n2"
$src  = "$root\src-tauri\src\router\score.rs"
$bak  = "$env:TEMP\score.base"

if (-not (Test-Path "$wt\src-tauri\Cargo.toml")) {
    Write-Output ("worktree missing: " + $wt)
    exit 2
}
# every run starts from the real sources, never from a previous edit
foreach ($rel in @('src-tauri\src\router\score.rs', 'src-tauri\tests\route_golden.rs',
                   'src-tauri\tests\route_golden_gen.rs', 'src-tauri\src\lib.rs',
                   'src-tauri\src\log_rotate.rs', 'src-tauri\tests\log_rotate.rs')) {
    $dst = Join-Path $wt $rel
    $dir = Split-Path -Parent $dst
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    Copy-Item (Join-Path $root $rel) $dst -Force
}
$fixtureDir = "$wt\src-tauri\tests\fixtures"
if (-not (Test-Path $fixtureDir)) { New-Item -ItemType Directory -Path $fixtureDir -Force | Out-Null }
Copy-Item "$root\src-tauri\tests\fixtures\route_golden.json" "$fixtureDir\route_golden.json" -Force

$cases = @(
    # WholeLine = true  -> swap the whole line payload, keep indentation.
    # WholeLine absent  -> swap only the matched token inside the line.
    @{ Label = 'Fastest latency weight 0.55 -> 0.05'; Find = 'latency: 0.55,'; New = 'latency: 0.05,'; WholeLine = $true },
    @{ Label = 'Reliable health weight 0.6 -> 0.2';   Find = 'health: 0.6,';   New = 'health: 0.2,';   WholeLine = $true },
    # Balanced and Custom and Weights::default all carry 'capability: 0.2,'.
    # Change all three on purpose: every strategy with that weight must be
    # probed, otherwise "only Balanced was tested" hides behind a green run.
    @{ Label = 'capability weight 0.2 -> 0.02 (balanced+custom+default)'; Find = 'capability: 0.2,'; ReplaceN = 3; New = 'capability: 0.02,'; WholeLine = $true },
    @{ Label = 'Smartest capability weight 0.6 -> 0.02'; Find = 'capability: 0.6,'; New = 'capability: 0.02,'; WholeLine = $true },
    # Token swap: the expression around it must survive.
    @{ Label = 'exact_match bonus 1.25 -> 1.0'; Find = '{ 1.25 }'; New = '{ 1.0 }' },
    # Both arms matter: the exemption threshold AND the decay constant.
    # Editing only the threshold is a no-op, because (2000.0 / ms) for
    # ms <= 2000 is still >= 1.0 and clamps back to 1.0. An earlier probe
    # changed only the threshold and reported GREEN, which said nothing.
    @{ Label = 'latency exemption 2000ms -> 200ms (two edits)';
       Find = 'n if n <= 2_000 => 1.0,'; New = 'n if n <= 200 => 1.0,' },
    @{ Label = 'latency decay constant 2000 -> 200';
       Find = '(2000.0 / (n as f32))'; New = '(200.0 / (n as f32))' },
    # The last two are expected NOT to flip anything. They are listed so the
    # matrix records why, instead of leaving a silent hole.
    @{ Label = 'Balanced health weight 0.35 -> 0.36 (too small to flip)';
       Find = 'health: 0.35,'; ReplaceN = 2; New = 'health: 0.36,'; WholeLine = $true },
    @{ Label = 'long-context bonus 0.15 -> 0.30 (already clamped at 1.0)';
       Find = 'n if n >= 200_000 => 0.15,'; New = 'n if n >= 200_000 => 0.30,' }
)

Write-Output '=== negative control matrix (route_golden) ==='
$red = 0
$green = 0
foreach ($c in $cases) {
    Copy-Item $src $bak -Force
    $lines = [System.IO.File]::ReadAllLines($bak, [System.Text.Encoding]::UTF8)
    $out = New-Object System.Collections.ArrayList
    $n = 0
    $want = 1
    if ($c.ContainsKey('ReplaceN')) { $want = [int]$c.ReplaceN }
    foreach ($one in $lines) {
        if ($one.Contains($c.Find)) {
            if ($c.ContainsKey('WholeLine')) {
                # replace the entire payload, keeping the original indentation
                $indent = $one.Substring(0, $one.Length - $one.TrimStart().Length)
                [void]$out.Add($indent + $c.New)
            } else {
                # replace just the token, so the surrounding expression survives
                [void]$out.Add($one.Replace($c.Find, $c.New))
            }
            $n++
            continue
        }
        [void]$out.Add($one)
    }
    if ($n -eq 0) {
        Write-Output ("  [SKIP-NOMATCH] " + $c.Label)
        continue
    }
    if ($n -ne $want) {
        Write-Output ("  [SKIP-HITS=" + $n + "] " + $c.Label + " -- expected " + $want + " hit(s)")
        continue
    }
    [System.IO.File]::WriteAllLines($bak, [string[]]$out, (New-Object System.Text.UTF8Encoding($false)))
    # CLAUDE.md rule 15: touch, or cargo reuses the old rlib and reports green
    $b = [System.IO.File]::ReadAllBytes($bak)
    [System.IO.File]::WriteAllBytes($bak, $b)
    Copy-Item $bak "$wt\src-tauri\src\router\score.rs" -Force

    & "$root\scripts\cargo-env.ps1" | Out-Null
    Set-Location $wt
    $r = cargo test --manifest-path "$wt\src-tauri\Cargo.toml" --test route_golden 2>&1 | Out-String
    $ec = $LASTEXITCODE
    Set-Location $root

    $compileErr = ($r -match 'error\[E') -or ($r -match 'could not compile')
    if ($compileErr) {
        Write-Output ("  [BROKEN-EDIT] " + $c.Label + " -- the edit does not compile, not a valid probe")
        continue
    }
    if ($ec -ne 0) {
        $red++
        $mark = 'RED-OK '
    } else {
        $green++
        $mark = 'GREEN   '
    }
    Write-Output ("  [$mark] " + $c.Label)
    foreach ($line in ($r -split "`r?`n")) {
        if ($line -match 'test result: FAILED') { Write-Output ('         ' + $line.Trim()) }
    }
}
Copy-Item $src "$wt\src-tauri\src\router\score.rs" -Force
Write-Output ("=== done: " + $red + " red, " + $green + " green ===")
exit 0
