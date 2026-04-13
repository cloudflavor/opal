# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=docker.io/library/rust:1.90
FROM ${RUST_IMAGE} AS build-linux

WORKDIR /work

# Comma-separated target list, for example:
# x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu
ARG RELEASE_TARGETS=x86_64-unknown-linux-gnu
ARG CI_JOB_NAME_SLUG=release-artifacts-docker

ENV CARGO_TERM_COLOR=always

COPY . .

RUN set -euo pipefail; \
    version="$(awk -F '"' '\
      $0 == "[package]" { in_package = 1; next }\
      /^\[/ && $0 != "[package]" { in_package = 0 }\
      in_package && $1 ~ /^version = / { print $2; exit }\
    ' crates/opal/Cargo.toml)"; \
    if [ -z "${version}" ]; then \
      echo "error: failed to read crates/opal/Cargo.toml package version" >&2; \
      exit 1; \
    fi; \
    export CI_COMMIT_TAG="v${version}"; \
    export RELEASE_TARGETS="${RELEASE_TARGETS}"; \
    export CI_JOB_NAME_SLUG="${CI_JOB_NAME_SLUG}"; \
    bash ./scripts/build-release-artifacts.sh

FROM scratch AS release-artifacts
COPY --from=build-linux /work/releases/ /releases/
