#!/usr/bin/env bash
set -euo pipefail

IMAGE_NAME="dry-exec-test-harness"

echo "==> Building Linux verification container image: ${IMAGE_NAME}..."
docker build -t "${IMAGE_NAME}" -f docker/Dockerfile.ci .

echo "==> Image built. To execute the complete verification suite inside the container:"
echo "    docker run --rm --privileged --cap-add=SYS_ADMIN --cap-add=SYS_PTRACE ${IMAGE_NAME}"
