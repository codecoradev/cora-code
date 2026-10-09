#!/usr/bin/env sh
# cora installer — https://github.com/codecoradev/cora-code
# Usage: curl -fsSL https://raw.githubusercontent.com/codecoradev/cora-code/main/install.sh | sh

set -e

REPO="codecoradev/cora-code"
BINARY_NAME="cora"
INSTALL_DIR="${CORA_INSTALL_DIR:-$HOME/.local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    printf "${GREEN}[INFO]${NC} %s\n" "$1"
}

warn() {
    printf "${YELLOW}[WARN]${NC} %s\n" "$1"
}

error() {
    printf "${RED}[ERROR]${NC} %s\n" "$1"
    exit 1
}

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Linux*)  OS="linux";;
        Darwin*) OS="darwin";;
        *)       error "Unsupported operating system: $(uname -s)";;
    esac
}

# Detect architecture
detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  ARCH="x86_64";;
        arm64|aarch64) ARCH="aarch64";;
        *)             error "Unsupported architecture: $(uname -m)";;
    esac
}

# Get latest release version
# Primary: parse the 302 redirect on /releases/latest (no API call, no rate limit).
# Fallback: the GitHub REST API (subject to 60 req/hour anonymous limit).
get_latest_version() {
    # Try the web redirect first — does not count against the API rate limit.
    VERSION=$(curl -sI --connect-timeout 10 --max-time 30 "https://github.com/${REPO}/releases/latest" \
        | grep -i '^location:' \
        | sed -E 's|.*/tag/([^[:space:]]+).*|\1|' \
        | tr -d '\r')

    # Fallback to the REST API if the redirect didn't yield a tag.
    if [ -z "$VERSION" ]; then
        warn "Redirect lookup failed, falling back to GitHub API..."
        VERSION=$(curl -fsSL --connect-timeout 10 --max-time 30 --max-filesize 1048576 "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name":' \
            | sed -E 's/.*"([^"]+)".*/\1/')
    fi

    if [ -z "$VERSION" ]; then
        error "Failed to get latest version. Set CORA_VERSION=vX.Y.Z to pin a version."
    fi
}

# Build target triple and archive name
get_target() {
    case "$OS" in
        linux)
            case "$ARCH" in
                x86_64)  TARGET="x86_64-unknown-linux-gnu";;
                aarch64) TARGET="aarch64-unknown-linux-gnu";;
            esac
            ;;
        darwin)
            # Only aarch64 (Apple Silicon) is currently published
            if [ "$ARCH" != "aarch64" ]; then
                warn "No pre-built binary for x86_64 macOS. Install via cargo:"
                warn "  cargo install --git https://github.com/${REPO}"
                exit 0
            fi
            TARGET="aarch64-apple-darwin"
            ;;
    esac
}

# Keyless cosign (Sigstore) identity that must have signed the release (#591).
COSIGN_IDENTITY_REGEXP="^https://github.com/${REPO}/\\.github/workflows/release\\.yml@refs/tags/v"
COSIGN_OIDC_ISSUER="https://token.actions.githubusercontent.com"

# Is CORA_REQUIRE_SIGNATURE on? Sets REQUIRE_SIG=1|0. Unknown values are an
# error rather than a silent "off" so a requested verification is never lost.
parse_require_signature() {
    case "${CORA_REQUIRE_SIGNATURE:-}" in
        ""|0|false) REQUIRE_SIG=0;;
        1|true)     REQUIRE_SIG=1;;
        *)          error "CORA_REQUIRE_SIGNATURE must be 1 or 0 (got '${CORA_REQUIRE_SIGNATURE}')";;
    esac
}

# Verify the cosign keyless signature over the checksums file.
# Policy (see docs/installation.md):
#   bundle present + cosign present -> verify; failure is fatal (fail closed)
#   cosign missing                  -> notice (checksum-only); fatal if REQUIRE_SIG=1
#   bundle absent (HTTP 404)        -> notice (older release); fatal if REQUIRE_SIG=1
#   any other bundle download error -> fatal
verify_signature() {
    BUNDLE_URL="${CHECKSUMS_URL}.sigstore.json"
    BUNDLE_FILE="${CHECKSUM_FILE}.sigstore.json"

    HTTP_CODE=$(curl -sSL --connect-timeout 10 --max-time 30 --max-filesize 1048576 \
        -w '%{http_code}' "$BUNDLE_URL" -o "$BUNDLE_FILE") || HTTP_CODE="000"
    case "$HTTP_CODE" in
        200) ;;
        404)
            if [ "$REQUIRE_SIG" = "1" ]; then
                error "No signature bundle published for ${VERSION} but CORA_REQUIRE_SIGNATURE=1. Refusing to install."
            fi
            warn "No cosign signature published for ${VERSION}; verified by SHA-256 checksum only."
            return 0
            ;;
        *) error "Failed to download signature bundle (HTTP ${HTTP_CODE}). Refusing to install (set CORA_SKIP_CHECKSUM=1 to bypass all verification, unsafe).";;
    esac

    if ! command -v cosign >/dev/null 2>&1; then
        if [ "$REQUIRE_SIG" = "1" ]; then
            error "cosign not found but CORA_REQUIRE_SIGNATURE=1. Install cosign: https://docs.sigstore.dev/cosign/system_config/installation/"
        fi
        warn "Signature NOT verified (checksum-only): cosign not found. Install it to verify: https://docs.sigstore.dev/cosign/system_config/installation/"
        return 0
    fi

    info "Verifying cosign signature..."
    if ! cosign verify-blob \
        --bundle "$BUNDLE_FILE" \
        --certificate-identity-regexp "$COSIGN_IDENTITY_REGEXP" \
        --certificate-oidc-issuer "$COSIGN_OIDC_ISSUER" \
        "$CHECKSUM_FILE" >/dev/null 2>&1; then
        error "cosign signature verification FAILED for checksums of ${VERSION}. Refusing to install."
    fi
    info "Signature verified (cosign keyless, GitHub Actions release workflow)"
}

# Download and install
install() {
    info "Detected: $OS $ARCH"
    info "Target: $TARGET"
    info "Version: $VERSION"

    ARCHIVE_NAME="${BINARY_NAME}-${TARGET}-${VERSION}.tar.gz"
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE_NAME}"
    TEMP_DIR=$(mktemp -d)
    ARCHIVE="${TEMP_DIR}/${ARCHIVE_NAME}"

    CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/checksums-sha256.txt"
    CHECKSUM_FILE="${TEMP_DIR}/checksums-sha256.txt"

    info "Downloading from: $DOWNLOAD_URL"
    if ! curl -fsSL --connect-timeout 10 --max-time 600 --max-filesize 268435456 "$DOWNLOAD_URL" -o "$ARCHIVE"; then
        error "Failed to download ${ARCHIVE_NAME}"
    fi

    # Verify SHA256 checksum (prevents MITM / corrupted download).
    # Mandatory: a missing checksums file or entry is fatal unless the user
    # explicitly opts out with CORA_SKIP_CHECKSUM=1.
    parse_require_signature
    if [ "${CORA_SKIP_CHECKSUM:-}" = "1" ]; then
        if [ "$REQUIRE_SIG" = "1" ]; then
            error "CORA_SKIP_CHECKSUM=1 conflicts with CORA_REQUIRE_SIGNATURE=1 (the signature covers the checksums file). Unset one."
        fi
        warn "CORA_SKIP_CHECKSUM=1 set - checksum verification DISABLED. The binary is NOT verified."
    else
        info "Downloading checksums..."
        if ! curl -fsSL --connect-timeout 10 --max-time 30 --max-filesize 1048576 "$CHECKSUMS_URL" -o "$CHECKSUM_FILE"; then
            error "Failed to download checksums. Refusing to install an unverified binary (set CORA_SKIP_CHECKSUM=1 to override, unsafe)."
        fi
        verify_signature
        info "Verifying SHA256 checksum..."
        # Exact filename match (optionally prefixed with '*' or './').
        EXPECTED=$(awk -v n="$ARCHIVE_NAME" '{f=$2; sub(/^\*/, "", f); sub(/^\.\//, "", f); if (f == n) {print $1; exit}}' "$CHECKSUM_FILE")
        if [ -z "$EXPECTED" ]; then
            error "Checksum for ${ARCHIVE_NAME} not found in checksums file. Refusing to install (set CORA_SKIP_CHECKSUM=1 to override, unsafe)."
        fi
        if command -v sha256sum >/dev/null 2>&1; then
            ACTUAL=$(sha256sum "$ARCHIVE" | awk '{print $1}')
        elif command -v shasum >/dev/null 2>&1; then
            ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
        else
            error "Neither sha256sum nor shasum found; cannot verify checksum (set CORA_SKIP_CHECKSUM=1 to override, unsafe)."
        fi
        if [ "$ACTUAL" != "$EXPECTED" ]; then
            error "Checksum mismatch! Expected: ${EXPECTED}, got: ${ACTUAL}"
        fi
        info "Checksum verified: $EXPECTED"
    fi

    # Verify archive contents before extraction (CWE-22 path traversal).
    # Reject any entry with an absolute path or a ".." component.
    info "Verifying archive integrity..."
    if tar -tzf "$ARCHIVE" | grep -qE '^/|(^|/)\.\.(/|$)'; then
        error "Archive contains unsafe paths (absolute or directory traversal) — refusing to extract"
    fi

    # Reject symlink/hardlink entries (first char of the verbose mode string:
    # 'l' = symlink; bsdtar lists hardlinks with 'h').
    if tar -tvzf "$ARCHIVE" | grep -qE '^[lh]'; then
        error "Archive contains symlink/hardlink entries - refusing to extract"
    fi

    info "Extracting..."
    # Extract only the single binary entry (tolerate an optional ./ prefix).
    tar -xzf "$ARCHIVE" -C "$TEMP_DIR" "${BINARY_NAME}" 2>/dev/null \
        || tar -xzf "$ARCHIVE" -C "$TEMP_DIR" "./${BINARY_NAME}" \
        || error "Binary '${BINARY_NAME}' not found in archive"

    mkdir -p "$INSTALL_DIR"
    mv "${TEMP_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/"

    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

    # macOS Gatekeeper workaround (#313): strip quarantine/provenance xattrs
    # that the browser/curl attaches to downloaded binaries. Without this,
    # macOS kills the unsigned binary with `Killed: 9` and no error message.
    if [ "$OS" = "darwin" ] && command -v xattr >/dev/null 2>&1; then
        # Best-effort — failures here are non-fatal (binary may already be clean).
        xattr -dr com.apple.quarantine "${INSTALL_DIR}/${BINARY_NAME}" 2>/dev/null || true
        xattr -dr com.apple.provenance "${INSTALL_DIR}/${BINARY_NAME}" 2>/dev/null || true
        info "Stripped macOS quarantine attributes (Gatekeeper workaround)"
    fi

    # Cleanup
    rm -rf "$TEMP_DIR"

    info "Successfully installed ${BINARY_NAME} to ${INSTALL_DIR}/${BINARY_NAME}"
}

# Verify installation
verify() {
    if command -v "$BINARY_NAME" >/dev/null 2>&1; then
        INSTALLED_VERSION=$("$BINARY_NAME" --version 2>/dev/null || echo "unknown")
        info "Verification: $INSTALLED_VERSION"
    else
        warn "Binary installed but not in PATH. Add to your shell profile:"
        case "${SHELL:-}" in
            */zsh)
                warn '  echo '\''export PATH="$HOME/.local/bin:$PATH"'\'' >> ~/.zshrc'
                warn '  source ~/.zshrc'
                ;;
            */bash)
                warn '  echo '\''export PATH="$HOME/.local/bin:$PATH"'\'' >> ~/.bashrc'
                warn '  source ~/.bashrc'
                ;;
            */fish)
                warn '  fish_add_path ~/.local/bin'
                ;;
            *)
                warn '  export PATH="$HOME/.local/bin:$PATH"'
                ;;
        esac
    fi
}

main() {
    info "Installing ${BINARY_NAME}..."

    detect_os
    detect_arch
    get_target
    if [ -n "$CORA_VERSION" ]; then
        VERSION="$CORA_VERSION"
        info "Using pinned version from CORA_VERSION: $VERSION"
    else
        get_latest_version
    fi
    install
    verify

    echo ""
    info "Installation complete! Run '${BINARY_NAME} --help' to get started."
}

main
