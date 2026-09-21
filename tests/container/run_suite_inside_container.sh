#!/usr/bin/env bash
set -euo pipefail

echo "================================================================="
echo " Phase 1: Running Rust Core Integration Tests (Loop 1 & Loop 2)..."
echo "================================================================="
cargo test --test loop1_loop2_tests -- --nocapture

echo "================================================================="
echo " Phase 2: Building Python SDK & PyO3 FFI Extension (Loop 3)..."
echo "================================================================="
maturin develop

echo "================================================================="
echo " Phase 3: Running Python SDK Test Suite (Loop 3 Assertions)..."
echo "================================================================="
pytest -v tests/test_sdk.py

echo "================================================================="
echo " Phase 4: Running CLI & Observability Tests (Loops 5 & 6)..."
echo "================================================================="
pytest -v tests/test_cli.py

echo "================================================================="
echo " Phase 5: Running Production-Ready Examples (Loop 7)..."
echo "================================================================="
pytest -v tests/test_examples.py

echo "================================================================="
echo " Phase 6: Running System-One Decision Layer Suite (Loop 13)..."
echo "================================================================="
pytest -v tests/test_decision.py

