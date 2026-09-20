#!/usr/bin/env bash
set -euo pipefail

echo "================================================================="
echo " Building dry-exec containerized verification image..."
echo "================================================================="
docker build -f tests/container/Dockerfile -t dry-exec-verifier .

echo "================================================================="
echo " Executing deterministic Linux kernel boundary test harness..."
echo "================================================================="
docker run --rm \
    --cap-add=SYS_ADMIN \
    --cap-add=SYS_PTRACE \
    --security-opt seccomp=unconfined \
    dry-exec-verifier
