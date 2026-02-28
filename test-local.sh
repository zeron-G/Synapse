#!/usr/bin/env bash
# Local test script — mirrors CI (Linux/WSL)
# Usage: ./test-local.sh
set -e

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

ok()   { echo -e "${GREEN}✅ $1${NC}"; }
fail() { echo -e "${RED}❌ $1${NC}"; exit 1; }
info() { echo -e "${YELLOW}→ $1${NC}"; }

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Synapse Local Test (WSL/Linux)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ── fmt ──────────────────────────────────
info "cargo fmt --check (core)"
(cd core && $CARGO fmt --check) && ok "core fmt" || fail "core fmt"

info "cargo fmt --check (idl)"
(cd idl && $CARGO fmt --check) && ok "idl fmt" || fail "idl fmt"

# ── clippy ───────────────────────────────
info "cargo clippy (core)"
(cd core && $CARGO clippy -- -D warnings 2>&1) && ok "core clippy" || fail "core clippy"

info "cargo clippy (idl)"
(cd idl && $CARGO clippy -- -D warnings 2>&1) && ok "idl clippy" || fail "idl clippy"

# ── test ─────────────────────────────────
info "cargo test (core)"
(cd core && $CARGO test --verbose 2>&1) && ok "core tests" || fail "core tests"

info "cargo test (idl)"
(cd idl && $CARGO test --verbose 2>&1) && ok "idl tests" || fail "idl tests"

echo ""
echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "  All checks passed! ✨"
echo -e "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
