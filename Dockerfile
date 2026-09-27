# syntax=docker/dockerfile:1

# The job image: everything a round needs except the repository's own
# toolchain, which `job-exec` provisions with mise at the start of the round.

# rust-toolchain.toml pins channel 1.98.1, not the crate's MSRV — match it
# here so rustup does not try to fetch a second toolchain mid-build.
FROM rust:1.98.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin assembly

FROM debian:bookworm-slim

# Every `curl | sh` below fails the build when curl does, rather than piping
# an empty script into a shell that exits 0.
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git xz-utils unzip \
 && rm -rf /var/lib/apt/lists/*

# Claude Code refuses to skip permission prompts as root, and an agent has
# no business being root anyway.
RUN useradd --create-home --uid 1000 agent \
 && mkdir -p /mise \
 && chown agent:agent /mise

# /mise is where job-exec, running as agent, installs the round's toolchain.
#
# opencode's installer puts its binary at ~/.opencode/bin regardless of
# --no-modify-path, so that directory joins ~/.local/bin (Claude Code's and
# Codex's) on PATH.
ENV MISE_DATA_DIR=/mise \
    MISE_CACHE_DIR=/mise/cache \
    MISE_YES=1 \
    PATH=/mise/shims:/home/agent/.local/bin:/home/agent/.opencode/bin:/usr/local/bin:/usr/bin:/bin

# Pinned, and bumped by renovate. Declared just before its use, so a bump
# rebuilds only the layers after it.
ARG MISE_VERSION=v2026.9.0
RUN curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise MISE_VERSION=${MISE_VERSION} sh

COPY --from=build /src/target/release/assembly /usr/local/bin/assembly

# Numeric, so a cluster that requires runAsNonRoot can verify it.
USER 1000:1000
WORKDIR /home/agent

# Not pinned: each build takes the agents' latest releases, so rebuilding the
# same tag can change them. Pass --build-arg to hold one at a version.
ARG CLAUDE_CODE_VERSION=latest
ARG CODEX_VERSION=latest
ARG OPENCODE_VERSION=latest

# Native builds, not npm: an npm-installed agent runs on whatever `node` the
# repository's mise config pins, which may be one the agent does not support.
#
# opencode's installer only accepts a bare version number (e.g. "1.0.180") in
# $VERSION — passing it the literal string "latest" makes it look for a
# release tagged "vlatest" and fail. Leaving $VERSION unset is what selects
# its own latest release, so that case is passed no version at all.
#
# GitHub's asset URL takes a different shape for "latest" than for a pinned
# tag: .../releases/latest/download/<asset> for the former,
# .../releases/download/<tag>/<asset> for the latter — "latest" is never a
# real tag name to substitute into the pinned form.
RUN curl -fsSL https://claude.ai/install.sh | bash -s -- ${CLAUDE_CODE_VERSION} \
 && if [ "${OPENCODE_VERSION}" = "latest" ]; then \
      curl -fsSL https://opencode.ai/install | bash; \
    else \
      curl -fsSL https://opencode.ai/install | VERSION=${OPENCODE_VERSION} bash; \
    fi \
 && mkdir -p /home/agent/.local/bin \
 && if [ "${CODEX_VERSION}" = "latest" ]; then \
      codex_url="https://github.com/openai/codex/releases/latest/download/codex-$(uname -m)-unknown-linux-musl.tar.gz"; \
    else \
      codex_url="https://github.com/openai/codex/releases/download/${CODEX_VERSION}/codex-$(uname -m)-unknown-linux-musl.tar.gz"; \
    fi \
 && curl -fsSL "$codex_url" | tar -xz -C /home/agent/.local/bin \
 && mv /home/agent/.local/bin/codex-* /home/agent/.local/bin/codex

# Fail the build, not the first job, if anything above is missing.
RUN assembly --version && git --version && mise --version \
 && claude --version && codex --version && opencode --version

ENTRYPOINT []
CMD ["assembly", "job-exec"]
