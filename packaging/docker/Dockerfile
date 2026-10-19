# Postvak's daemon in a container; .github/workflows/docker.yml builds it. The build context holds postvak-amd64 and
# postvak-arm64, built without the GUI, and an empty data/. No RUN step, so both architectures build without emulation.
FROM debian:trixie-slim

ARG TARGETARCH
LABEL org.opencontainers.image.title="Postvak" \
      org.opencontainers.image.description="Postvak's background sync: IMAP mailbox rules, headless" \
      org.opencontainers.image.source="https://github.com/postvak-app/postvak" \
      org.opencontainers.image.documentation="https://postvak.pataar.nl/daemon.html#docker" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

COPY --chmod=755 postvak-${TARGETARCH} /usr/local/bin/postvak
# Owned by the user below, so a named volume starts writable; a bind mount keeps the host directory's owner.
COPY --chown=1000:1000 data/ /data/

# config.toml and rules.toml in /data/config, the mail stores in /data/state.
ENV POSTVAK_HOME=/data
VOLUME /data
WORKDIR /data
# No passwd entry is needed: POSTVAK_HOME stands in for the home directory.
USER 1000:1000

ENTRYPOINT ["postvak"]
CMD ["run"]
