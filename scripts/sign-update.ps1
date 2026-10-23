<#
.SYNOPSIS
  Sign the exact installer checksum bytes with the Ed25519 UPDATE_SIGNING_KEY PEM secret.
#>
param(
    [Parameter(Mandatory = $true)][string]$Checksum,
    [string]$VerifierSource = (Join-Path $PSScriptRoot '../src/platform/update.rs')
)
$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($env:UPDATE_SIGNING_KEY)) {
    throw 'UPDATE_SIGNING_KEY is required: refusing to publish an unsigned update.'
}
$source = Get-Content -LiteralPath $VerifierSource -Raw
$match = [regex]::Match($source, 'const UPDATE_VERIFYING_KEY: Option<&str>\s*=\s*Some\("([0-9a-fA-F]{64})"\);')
if (-not $match.Success) { throw 'Configure UPDATE_VERIFYING_KEY before publishing updates.' }
$checksumPath = (Resolve-Path -LiteralPath $Checksum).Path
$signaturePath = "$checksumPath.sig"
$temporary = Join-Path ([IO.Path]::GetTempPath()) ("tile-rs-sign-update-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
$privateKey = Join-Path $temporary 'signing.pem'
$publicKey = Join-Path $temporary 'public.der'
try {
    [IO.File]::WriteAllText($privateKey, $env:UPDATE_SIGNING_KEY, [Text.UTF8Encoding]::new($false))
    & openssl pkey -in $privateKey -pubout -outform DER -out $publicKey
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the update signing key.' }
    $der = [IO.File]::ReadAllBytes($publicKey)
    $derHex = ($der | ForEach-Object { $_.ToString('x2') }) -join ''
    # RFC 8410 SubjectPublicKeyInfo for a raw 32-byte Ed25519 key.
    if ($derHex -notmatch '^302a300506032b6570032100([0-9a-f]{64})$') {
        throw 'UPDATE_SIGNING_KEY must be an Ed25519 PEM private key.'
    }
    if ($Matches[1] -ne $match.Groups[1].Value.ToLowerInvariant()) {
        throw 'UPDATE_SIGNING_KEY does not match the public key embedded in tile-rs.'
    }
    & openssl pkeyutl -sign -rawin -inkey $privateKey -in $checksumPath -out $signaturePath
    if ($LASTEXITCODE -ne 0) { throw 'Update checksum signing failed.' }
    if ((Get-Item -LiteralPath $signaturePath).Length -ne 64) { throw 'Invalid Ed25519 signature size.' }
    & openssl pkeyutl -verify -rawin -pubin -inkey $publicKey -keyform DER -in $checksumPath -sigfile $signaturePath
    if ($LASTEXITCODE -ne 0) { throw 'Update checksum signature verification failed.' }
} catch {
    Remove-Item -LiteralPath $signaturePath -ErrorAction SilentlyContinue
    throw
} finally {
    Remove-Item -LiteralPath $privateKey, $publicKey -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $temporary -ErrorAction SilentlyContinue
}
