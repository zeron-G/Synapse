#!/usr/bin/env bash
# Local test script — mirrors CI (Linux/WSL)
# Usage: ./test-local.sh
set -e

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

ok()   { echo -e "${GREEN}✅ $1${NC}"; }
fail() { echo -e "${RED}❌ $1${NC}"; exit 1; }
info() { echo -e "${YELLOW}→ $1${NC}"; }

PASS=0; SKIP=0

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Synapse Local Test (WSL/Linux)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ── fmt ──────────────────────────────────
info "cargo fmt --check (core)"
(cd core && $CARGO fmt --check) && ok "core fmt" || fail "core fmt"
PASS=$((PASS+1))

info "cargo fmt --check (idl)"
(cd idl && $CARGO fmt --check) && ok "idl fmt" || fail "idl fmt"
PASS=$((PASS+1))

# ── clippy ───────────────────────────────
info "cargo clippy (core)"
(cd core && $CARGO clippy -- -D warnings 2>&1) && ok "core clippy" || fail "core clippy"
PASS=$((PASS+1))

info "cargo clippy (idl)"
(cd idl && $CARGO clippy -- -D warnings 2>&1) && ok "idl clippy" || fail "idl clippy"
PASS=$((PASS+1))

# ── Rust tests ────────────────────────────
info "cargo test (core — unit + integration + Phase 2)"
(cd core && $CARGO test --verbose 2>&1) && ok "core tests" || fail "core tests"
PASS=$((PASS+1))

info "cargo test (idl)"
(cd idl && $CARGO test --verbose 2>&1) && ok "idl tests" || fail "idl tests"
PASS=$((PASS+1))

# ── Benchmark compilation check ──────────
info "cargo bench --no-run (core)"
(cd core && $CARGO bench --no-run 2>&1) && ok "benchmarks compile" || fail "benchmarks compile"
PASS=$((PASS+1))

# ── synapse CLI smoke test ────────────────
info "synapse compile CLI"
(cd idl && $CARGO build --bin synapse 2>&1) && ok "synapse CLI builds" || fail "synapse CLI build"
PASS=$((PASS+1))

SYNAPSE="./idl/target/debug/synapse"
TMPOUT=$(mktemp -d)
$SYNAPSE compile idl/examples/game.bridge --lang rust python cpp --output "$TMPOUT" \
    && ok "synapse compile game.bridge" || fail "synapse compile game.bridge"
PASS=$((PASS+1))

[ -f "$TMPOUT/game.rs" ] && [ -f "$TMPOUT/game.py" ] && [ -f "$TMPOUT/game.hpp" ] \
    && ok "all 3 outputs generated" || fail "missing output files"
PASS=$((PASS+1))

$SYNAPSE compile idl/examples/sensors.bridge --lang rust --output "$TMPOUT" \
    && ok "synapse compile sensors.bridge" || fail "synapse compile sensors.bridge"
PASS=$((PASS+1))
rm -rf "$TMPOUT"

# ── Python bridge tests ───────────────────
info "Python bridge tests"
if command -v python3 >/dev/null 2>&1; then
    python3 examples/test_python_bridge.py && ok "Python bridge tests" || fail "Python bridge tests"
    PASS=$((PASS+1))
else
    echo -e "${YELLOW}  python3 not found — skipping Python tests${NC}"
    SKIP=$((SKIP+1))
fi

# ── C++ compilation + runtime ─────────────
info "C++ compilation check (cpp_receiver.cpp)"
if command -v g++ >/dev/null 2>&1; then
    g++ -std=c++17 -O2 -Ibindings/cpp/include -lrt \
        -o /tmp/cpp_receiver examples/cpp_receiver.cpp \
        && ok "C++ cpp_receiver compiles" || fail "C++ cpp_receiver compilation"
    PASS=$((PASS+1))

    info "C++ header test (synapse_cpp_test.cpp)"
    g++ -std=c++17 -O2 -Ibindings/cpp/include -lrt \
        -o /tmp/synapse_cpp_test tests/synapse_cpp_test.cpp \
        && ok "C++ header test compiles" || fail "C++ header test compilation"
    PASS=$((PASS+1))

    info "Running C++ header test"
    /tmp/synapse_cpp_test && ok "C++ header test passed" || fail "C++ header test"
    PASS=$((PASS+1))
else
    echo -e "${YELLOW}  g++ not found — skipping C++ tests${NC}"
    SKIP=$((SKIP+3))
fi

# ── Summary ───────────────────────────────
echo ""
echo -e "${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "  All checks passed! ($PASS passed, $SKIP skipped)"
echo -e "  Rust: fmt + clippy + unit + integration + Phase 2"
echo -e "  Benchmarks: compilation verified"
echo -e "  Python: bridge wire-format + e2e messaging tests"
echo -e "  C++: compilation check + header runtime tests"
echo -e "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
