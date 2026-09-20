# Containerized verification runtime environment for dry-exec Linux kernel primitives and Python SDK
FROM rust:1.85-bookworm

WORKDIR /workspace

# Install kernel diagnostic utilities, Python 3, and virtual environment tooling
RUN apt-get update && apt-get install -y \
    build-essential \
    strace \
    libcap-dev \
    python3 \
    python3-pip \
    python3-venv \
    && rm -rf /var/lib/apt/lists/*

# Setup isolated virtual environment
ENV VIRTUAL_ENV=/opt/venv
RUN python3 -m venv $VIRTUAL_ENV
ENV PATH="$VIRTUAL_ENV/bin:$PATH"

RUN pip install --no-cache-dir maturin pytest pytest-asyncio pydantic

COPY . /workspace

RUN chmod +x /workspace/tests/container/run_suite_inside_container.sh

# Validate compilation and execute deterministic verification suite
CMD ["/workspace/tests/container/run_suite_inside_container.sh"]
