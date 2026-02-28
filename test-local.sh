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

# ── Rust tests ────────────────────────────
info "cargo test (core)"
(cd core && $CARGO test --verbose 2>&1) && ok "core tests" || fail "core tests"

info "cargo test (idl)"
(cd idl && $CARGO test --verbose 2>&1) && ok "idl tests" || fail "idl tests"

# ── Python bridge tests ───────────────────
info "Python bridge tests"
if command -v python3 >/dev/null 2>&1; then
    python3 examples/test_python_bridge.py && ok "Python bridge tests" || fail "Python bridge tests"
else
    echo -e "${YELLOW}  python3 not found — skipping Python tests${NC}"
fi

# ── C++ compilation + runtime ─────────────
info "C++ compilation check (cpp_receiver.cpp)"
if command -v g++ >/dev/null 2>&1; then
    g++ -std=c++17 -O2 -Ibindings/cpp/include -lrt \
        -o /tmp/cpp_receiver examples/cpp_receiver.cpp \
        && ok "C++ cpp_receiver compiles" || fail "C++ cpp_receiver compilation"

    info "C++ header test (synapse_cpp_test.cpp)"
    g++ -std=c++17 -O2 -Ibindings/cpp/include -lrt \
        -o /tmp/synapse_cpp_test tests/synapse_cpp_test.cpp \
        && ok "C++ header test compiles" || fail "C++ header test compilation"

    info "Running C++ header test"
    /tmp/synapse_cpp_test && ok "C++ header test passed" || fail "C++ header test"
else
    echo -e "${YELLOW}  g++ not found — skipping C++ tests${NC}"
fi

# ── Summary ───────────────────────────────
echo ""
echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "  All checks passed! ✨"
echo -e "  Rust: fmt + clippy + unit + integration + cross-process"
echo -e "  Python: bridge wire-format + e2e messaging tests"
echo -e "  C++: compilation check + header runtime tests"
echo -e "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
