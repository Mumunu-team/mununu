#!/bin/bash
# Dependency auditing script for Mununu
# Checks for security vulnerabilities and outdated dependencies

set -e

echo "🔍 Running dependency security audit..."
echo ""

# Check if cargo-audit is installed
if ! command -v cargo-audit &> /dev/null; then
    echo "⚠️  cargo-audit is not installed."
    echo "   Install it with: cargo install cargo-audit"
    echo ""
    echo "   Alternatively, install via:"
    echo "   cargo install cargo-audit --locked"
    exit 1
fi

# Run security audit
echo "📋 Checking for security vulnerabilities (cargo audit)..."
# Check cargo-audit version and warn if outdated
AUDIT_VERSION=$(cargo audit --version 2>&1 | grep -oE 'v?[0-9]+\.[0-9]+\.[0-9]+' | head -1 || echo "unknown")
REQUIRED_VERSION="0.22.0"
if [ "$AUDIT_VERSION" != "unknown" ] && [ "$(printf '%s\n' "$REQUIRED_VERSION" "$AUDIT_VERSION" | sort -V | head -1)" != "$REQUIRED_VERSION" ]; then
    echo "⚠️  cargo-audit version $AUDIT_VERSION detected."
    echo "   Version 0.22.0+ is recommended for CVSS 4.0 support."
    echo "   Update with: cargo install cargo-audit --force"
    echo ""
fi

if cargo audit; then
    echo "✅ No security vulnerabilities found!"
else
    echo "❌ Security vulnerabilities detected!"
    echo "   Review the output above and update affected dependencies."
    exit 1
fi

echo ""
echo "📦 Checking for outdated dependencies..."
echo ""

# Check if cargo-outdated is installed
if ! command -v cargo-outdated &> /dev/null; then
    echo "⚠️  cargo-outdated is not installed."
    echo ""
    echo "   Install it with:"
    echo "   cargo install cargo-outdated --locked --version 0.19.0"
    echo ""
    echo "   Note: Requires Rust 1.91+ (project minimum: rust-version = \"1.91\")"
    echo ""
    echo "   To see outdated dependencies after installation, run:"
    echo "   cargo outdated"
    exit 0
fi

# Run outdated check
echo "📋 Checking for outdated dependencies (cargo outdated)..."
if cargo outdated --exit-code 1 2>/dev/null; then
    echo "✅ All dependencies are up to date!"
else
    echo "⚠️  Some dependencies have newer versions available."
    echo "   Review the output above and consider updating."
    echo ""
    echo "   To update all dependencies:"
    echo "   cargo update"
    echo ""
    echo "   To update a specific dependency:"
    echo "   cargo update -p <package-name>"
fi

echo ""
echo "✅ Dependency audit complete!"
