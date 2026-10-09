#!/bin/sh
# Decision-matrix test for install.sh verify_signature() (#591).
# No network: uses a stub curl and a stub cosign. Usage: sh scripts/test_install_signature.sh
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir "$T/with" "$T/without"

cat > "$T/with/curl" <<'STUB'
#!/bin/sh
out=""; w=""
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift;; -w) w="$2"; shift;; esac; shift; done
[ "$FAKE_CODE" = "ERR" ] && exit 28
[ "$FAKE_CODE" = "200" ] && echo bundle > "$out"
[ -n "$w" ] && printf '%s' "$FAKE_CODE"
exit 0
STUB
cat > "$T/with/cosign" <<'STUB'
#!/bin/sh
echo "$@" > "$COSIGN_ARGS"
exit "${FAKE_COSIGN_RC:-0}"
STUB
chmod +x "$T/with/curl" "$T/with/cosign"
cp "$T/with/curl" "$T/without/curl"
# install.sh without its trailing `main` call
sed '$d' "$ROOT/install.sh" > "$T/lib.sh"

FAIL=0
# check <name> <http> <cosign 1|0> <cosign rc> <REQUIRE> <expected rc> <expected output substring>
check() {
    name=$1; code=$2; has=$3; crc=$4; req=$5; want=$6; sub=$7
    if [ "$has" = 1 ]; then P="$T/with"; else P="$T/without"; fi
    out=$( (
        FAKE_CODE=$code FAKE_COSIGN_RC=$crc CORA_REQUIRE_SIGNATURE=$req COSIGN_ARGS="$T/args"
        export FAKE_CODE FAKE_COSIGN_RC CORA_REQUIRE_SIGNATURE COSIGN_ARGS
        PATH="$P:/usr/bin:/bin"
        # shellcheck disable=SC1091
        . "$T/lib.sh"
        VERSION=v9.9.9
        CHECKSUMS_URL=http://x/checksums-sha256.txt
        CHECKSUM_FILE="$T/checksums-sha256.txt"
        : > "$CHECKSUM_FILE"
        parse_require_signature
        verify_signature
    ) 2>&1 )
    rc=$?
    case "$out" in *"$sub"*) okout=1;; *) okout=0;; esac
    if [ "$rc" = "$want" ] && [ "$okout" = 1 ]; then
        echo "PASS $name"
    else
        echo "FAIL $name (rc=$rc want=$want) $out"
        FAIL=1
    fi
}

check "verified"                 200 1 0 ""  0 "Signature verified"
check "cosign rejects -> fatal"  200 1 1 ""  1 "FAILED"
check "cosign missing -> notice" 200 0 0 ""  0 "NOT verified"
check "cosign missing + REQUIRE" 200 0 0 1   1 "cosign not found"
check "no bundle -> notice"      404 1 0 ""  0 "checksum only"
check "no bundle + REQUIRE"      404 1 0 1   1 "CORA_REQUIRE_SIGNATURE=1"
check "HTTP 500 -> fatal"        500 1 0 ""  1 "Failed to download signature"
check "network error -> fatal"   ERR 1 0 ""  1 "Failed to download signature"
check "invalid env -> fatal"     200 1 0 yes 1 "must be 1 or 0"

# Re-run the success case so $T/args holds the verified invocation, then check flags.
check "verified (args)"          200 1 0 ""  0 "Signature verified"
if grep -q -- "--certificate-oidc-issuer https://token.actions.githubusercontent.com" "$T/args" \
   && grep -q -- 'release\\.yml@refs/tags/v' "$T/args" \
   && grep -q -- "--bundle" "$T/args"; then
    echo "PASS cosign args"
else
    echo "FAIL cosign args: $(cat "$T/args")"
    FAIL=1
fi

exit "$FAIL"
