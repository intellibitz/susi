# Disposable keys exercise the verifier. They do not authorize a real check.
param(
    [string] $Verifier = $env:SUSI_CHECKPOINT_VERIFIER,
    [string] $Subject = (Join-Path $PSScriptRoot '../..'),
    [string] $AuthorityBundle = $env:SUSI_CHECKPOINT_AUTHORITY_DIR,
    [string] $TrustedFingerprint = $env:SUSI_CHECKPOINT_PRIMARY_FINGERPRINT,
    [string] $Receipt,
    [string] $Publication
)
$ErrorActionPreference = 'Stop'
if (!$Verifier) { throw 'Supply the trusted verifier via -Verifier or SUSI_CHECKPOINT_VERIFIER' }
$Verifier = (Resolve-Path $Verifier).Path
$Subject = (Resolve-Path $Subject).Path
if ($AuthorityBundle) {
    $AuthorityBundle = (Resolve-Path $AuthorityBundle).Path
    if (!$Receipt) { $Receipt=Join-Path $AuthorityBundle 'native-receipt' }
    if (!$Publication) { $Publication=Join-Path $AuthorityBundle 'publication.json' }
}
if (!$Receipt -or !$Publication) { throw 'Supply receipt and publication, or a trusted authority bundle' }
$dir = Join-Path ([IO.Path]::GetTempPath()) ("susi-compliance-" + [guid]::NewGuid())
$keys = Join-Path $dir 'keys'
New-Item -ItemType Directory $keys | Out-Null
$env:GNUPGHOME = $keys
$statement = Join-Path $dir 'statement.json'
$signature = Join-Path $dir 'statement.sig'
$keyring = Join-Path $dir 'authorities.gpg'
$base = 'e6824c957b1c41e5c2611266da564f99bf620394'
$head = '216e3bb157b677827757723da6c5126cebdc63a5'
$context = @('intellibitz/susi','1299','main','codex/step1-qualified-0.21.43',$base,$head)
$subjectRepo = Join-Path $dir 'subject.git'
git clone -q --bare --shared --no-tags $Subject $subjectRepo
if ($LASTEXITCODE) { throw 'failed to construct read-only subject fixture' }
git -C $subjectRepo update-ref refs/pull/1299/head $head
if ($LASTEXITCODE) { throw 'failed to create private PR ref' }
$resolved = git -C $subjectRepo rev-parse refs/pull/1299/head
if ($resolved -ne $head) { throw 'private PR readback differs' }
$common = @($subjectRepo,$Receipt,$Publication,$statement)
function Check([string]$name, [int]$expected, [string[]]$argv) {
    & $Verifier @argv
    if ($LASTEXITCODE -ne $expected) { throw "$name returned $LASTEXITCODE; expected $expected" }
    Write-Output "PASS $name"
}
function Sign([string]$fingerprint) {
    & gpg --homedir $keys --batch --yes --pinentry-mode loopback --local-user $fingerprint --output $signature --detach-sign $statement
    if ($LASTEXITCODE) { throw 'fixture signing failed' }
}
Check 'reconstruct real checkpoint' 0 (@('statement') + $common + $context)
$canonical = [IO.File]::ReadAllBytes($statement)
foreach ($name in 'Reviewer','Untrusted') {
    # Use an explicit parameter file; no access to the operator's signing agent.
    $params = Join-Path $dir "$name.key-parameters"
    [IO.File]::WriteAllText($params, "%no-protection`nKey-Type: eddsa`nKey-Curve: ed25519`nKey-Usage: sign`nName-Real: $name`nName-Email: $name@fixture.invalid`nExpire-Date: 1d`n%commit`n")
    & gpg --homedir $keys --batch --generate-key $params
    if ($LASTEXITCODE) { throw 'cannot generate disposable signing key' }
}
$fingerprints = @(& gpg --homedir $keys --batch --with-colons --list-keys | Where-Object {$_ -like 'fpr:*'} | ForEach-Object {$_.Split(':')[9]})
if ($fingerprints.Count -ne 2) { throw 'expected two disposable primary fingerprints' }
& gpg --homedir $keys --batch --yes --output $keyring --export
if ($LASTEXITCODE) { throw 'cannot export public keyring' }
$trust = @($signature,$keyring,$fingerprints[0])
$verify = @('verify') + $common + $context + $trust
Sign $fingerprints[0]
Check 'independently signed complete checkpoint' 0 $verify
foreach ($case in @(
    @('repository replay',5,'another/susi'), @('PR replay',6,'1300'),
    @('base branch replay',7,'release'), @('feature branch replay',8,'other'),
    @('new base',9,$head), @('new head',10,$base)
)) {
    $changed = $verify.Clone()
    $changed[$case[1]] = $case[2]
    Check $case[0] 1 $changed
}
foreach ($case in @(
    @('tree','540c7bcdeb0be389eb167b71fcef4817f2b52802',$base),
    @('scoped path','docs/step1-acceptance.md','docs/unclaimed.md'),
    @('result','observed-passes=4242','observed-passes=0'),
    @('provenance','T-RELEASE43PROOF-1','T-UNRELATED-1')
)) {
    $old = [Text.Encoding]::UTF8.GetString($canonical)
    $new = $old.Replace($case[1],$case[2])
    if ($old -ceq $new) {throw "ineffective mutation $($case[0])"}
    [IO.File]::WriteAllText($statement,$new)
    Sign $fingerprints[0]
    Check "signed altered $($case[0])" 1 $verify
}
[IO.File]::WriteAllBytes($statement,$canonical)
Check 'signature for different bytes' 1 $verify
Sign $fingerprints[1]
Check 'signature by untrusted key even when key is in keyring' 1 $verify
Sign $fingerprints[0]
foreach ($i in 2,3) {
    $changed = $verify.Clone()
    $changed[$i] = Join-Path $dir "changed-$i"
    [IO.File]::WriteAllBytes($changed[$i],[IO.File]::ReadAllBytes($verify[$i]))
    [IO.File]::AppendAllText($changed[$i],' ')
    Check "altered evidence input $i" 1 $changed
}
$changed=$verify.Clone()
$changed[11]=Join-Path $dir 'missing.sig'
Check 'unsigned' 1 $changed
if ($AuthorityBundle) {
    if (!$TrustedFingerprint) { throw 'Set the independently approved primary fingerprint' }
    $real=@('verify',$subjectRepo,$Receipt,$Publication,(Join-Path $AuthorityBundle 'statement.json')) + $context +
        @((Join-Path $AuthorityBundle 'statement.sig'),(Join-Path $AuthorityBundle 'authority.gpg'),$TrustedFingerprint)
    Check 'authentic independent authority' 0 $real
    foreach ($case in @(@('PR',6,'1300'),@('base',9,$head),@('head',10,$base))) {
        $changed=$real.Clone()
        $changed[$case[1]]=$case[2]
        Check "authentic signature replay on changed $($case[0])" 1 $changed
    }
    $badSig=Join-Path $dir 'corrupt-authority.sig'
    & gpg --homedir $keys --batch --yes --output $badSig --dearmor $real[11]
    if ($LASTEXITCODE) { throw 'expected armored authority signature' }
    $packet=[IO.File]::ReadAllBytes($badSig)
    $packet[-1]=$packet[-1] -bxor 1
    [IO.File]::WriteAllBytes($badSig,$packet)
    $changed=$real.Clone()
    $changed[11]=$badSig
    Check 'real signature packet mutation' 1 $changed
}

# Exercise the unchanged nonnative gate with a live remote-style claim snapshot.
$legacy=Join-Path $dir 'legacy'
New-Item -ItemType Directory $legacy | Out-Null
$bash = if ($IsWindows) { Join-Path $env:ProgramFiles 'Git/usr/bin/bash.exe' } else { 'bash' }
if ($IsWindows) { $env:PATH = (Split-Path $bash) + ';' + $env:PATH }
Push-Location $legacy
try {
    git init -q -b main
    git config user.name Fixture
    git config user.email fixture@localhost
    git config commit.gpgsign false
    git -c core.hooksPath= commit -qm seed --allow-empty
    New-Item -ItemType Directory scripts,'.agents/tasks' | Out-Null
    foreach ($name in 'check-workflow-compliance.sh','posix-compat.sh','owed-task-receipts.sh') {
        $lines = git -C $Subject show "${base}:scripts/$name"
        if ($LASTEXITCODE) { throw "missing base-owned legacy script $name" }
        [IO.File]::WriteAllText((Join-Path $legacy "scripts/$name"), (($lines -join "`n")+"`n"))
    }
    [IO.File]::WriteAllText((Join-Path $legacy '.agents/tasks/T-BRIDGE-1.json'),'{"id":"T-BRIDGE-1","accept":{"cmd":["cargo","test"]}}')
    git add .
    git -c core.hooksPath= commit -qm 'install base checker'
    $legacyBase=git rev-parse HEAD
    git update-ref refs/remotes/origin/main $legacyBase
    git checkout -qb fixture
    $claim=@{task='T-BRIDGE-1';agent='BRIDGE';branch='fixture';scopes=@('work');lease_until_unix=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()+600}|ConvertTo-Json -Compress
    $blob=$claim|git hash-object -w --stdin
    git update-ref refs/claims/T-BRIDGE-1 $blob
    [IO.File]::WriteAllText((Join-Path $legacy 'work'),'one')
    git add work
    git -c core.hooksPath= commit -qm 'legacy work' -m 'Task: T-BRIDGE-1'
    $env:SUSI_WORKFLOW_REFS_FETCHED='1'
    & $bash scripts/check-workflow-compliance.sh $legacyBase HEAD origin fixture
    if ($LASTEXITCODE) {throw 'valid scoped legacy task rejected'}
    [IO.File]::WriteAllText((Join-Path $legacy 'work'),'two')
    git add work
    git -c core.hooksPath= commit -qm 'untrusted trailer' -m 'Task: LOCAL-checkpoint'
    & $bash scripts/check-workflow-compliance.sh $legacyBase HEAD origin fixture
    if ($LASTEXITCODE -ne 1) {throw 'unattested LOCAL trailer admitted'}
    Write-Output 'PASS legacy scoped T-task and refusal of bare LOCAL trailer'
} finally {Pop-Location}
Write-Output "Private fixture evidence: $dir"
