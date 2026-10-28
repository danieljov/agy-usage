#!/usr/bin/env bash
#
# Standalone installation script for agy-usage
# https://github.com/danieljov/agy-usage
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/danieljov/agy-usage/main/install.sh | bash
#
# Environment variables:
#   INSTALL_DIR   Target directory for binary [default: ~/.local/bin or /usr/local/bin if root]
#   VERSION       Specific release tag to install (e.g. v0.1.0) [default: latest]

set -euo pipefail

info() {
    printf "\033[1;34m::\033[0m %s\n" "$*"
}

error() {
    printf "\033[1;31merror:\033[0m %s\n" "$*" >&2
    exit 1
}

# 1. OS check
OS="$(uname -s)"
case "${OS}" in
    Linux)
        TARGET_OS="unknown-linux-gnu"
        ;;
    Darwin)
        TARGET_OS="apple-darwin"
        ;;
    *)
        error "Unsupported operating system: ${OS}. Build from source with: cargo install --git https://github.com/danieljov/agy-usage.git"
        ;;
esac

# 2. Architecture check
ARCH="$(uname -m)"
case "${ARCH}" in
    x86_64|amd64)
        TARGET_ARCH="x86_64"
        ;;
    aarch64|arm64)
        TARGET_ARCH="aarch64"
        ;;
    *)
        error "Unsupported architecture: ${ARCH}. Build from source with: cargo install --git https://github.com/danieljov/agy-usage.git"
        ;;
esac

TARGET_TRIPLE="${TARGET_ARCH}-${TARGET_OS}"
REPO="danieljov/agy-usage"

# 3. Determine version
if [ -z "${VERSION:-}" ]; then
    info "Resolving latest release from GitHub..."
    VERSION=$(curl -fsSL -o /dev/null -w "%{url_effective}" "https://github.com/${REPO}/releases/latest" | sed 's#.*/##')
    if [ -z "${VERSION}" ] || [ "${VERSION}" = "releases" ]; then
        error "Failed to resolve latest release tag from GitHub"
    fi
fi

info "Selected version: ${VERSION}"

# 4. Prepare temporary working directory
TMP_DIR="$(mktemp -d -t agy-usage-install-XXXXXX)"
cleanup() {
    rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

# 5. Download tarball and checksum
TARBALL="agy-usage-${VERSION}-${TARGET_TRIPLE}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
CHECKSUM_URL="${DOWNLOAD_URL}.sha256"

info "Downloading ${TARBALL}..."
curl -fsSL -o "${TMP_DIR}/${TARBALL}" "${DOWNLOAD_URL}"

info "Downloading SHA-256 checksum..."
curl -fsSL -o "${TMP_DIR}/${TARBALL}.sha256" "${CHECKSUM_URL}" || error "Failed to download SHA-256 checksum from ${CHECKSUM_URL}"

info "Verifying SHA-256 checksum..."
if command -v sha256sum >/dev/null 2>&1; then
    (cd "${TMP_DIR}" && sha256sum -c "${TARBALL}.sha256") >/dev/null 2>&1 || error "SHA-256 checksum verification failed for ${TARBALL}"
elif command -v shasum >/dev/null 2>&1; then
    (cd "${TMP_DIR}" && shasum -a 256 -c "${TARBALL}.sha256") >/dev/null 2>&1 || error "SHA-256 checksum verification failed for ${TARBALL}"
else
    error "Neither 'sha256sum' nor 'shasum' was found in PATH to verify package integrity"
fi
info "SHA-256 checksum verified successfully."

# 6. Extract archive
info "Extracting ${TARBALL}..."
tar -xzf "${TMP_DIR}/${TARBALL}" -C "${TMP_DIR}"

# Find extracted binary
EXTRACTED_BIN=$(find "${TMP_DIR}" -type f -name "agy-usage" | head -n 1)
if [ -z "${EXTRACTED_BIN}" ] || [ ! -f "${EXTRACTED_BIN}" ]; then
    error "Binary agy-usage not found in extracted archive"
fi

# 7. Determine installation destination
if [ -z "${INSTALL_DIR:-}" ]; then
    if [ "$(id -u)" -eq 0 ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="${HOME}/.local/bin"
    fi
fi

mkdir -p "${INSTALL_DIR}"
install -m 755 "${EXTRACTED_BIN}" "${INSTALL_DIR}/agy-usage"
info "Installed agy-usage to ${INSTALL_DIR}/agy-usage"

# 8. Check PATH
case ":${PATH}:" in
    *:"${INSTALL_DIR}":*)
        ;;
    *)
        printf "\n\033[1;33mwarning:\033[0m %s is not in your PATH.\n" "${INSTALL_DIR}"
        printf "To use 'agy-usage' directly, add it to your PATH by adding this line to your ~/.bashrc or ~/.zshrc:\n"
        printf "  export PATH=\"%s:\$PATH\"\n\n" "${INSTALL_DIR}"
        ;;
esac

# 9. Next steps
printf "\n\033[1;32m✓ agy-usage %s installed successfully!\033[0m\n\n" "${VERSION}"
printf "To enable automatic 5-minute background sampling, run:\n"
printf "  agy-usage install-timer\n\n"
printf "To launch the terminal dashboard, run:\n"
printf "  agy-usage show\n"
