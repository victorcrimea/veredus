# Packages the binaries the release workflow already built instead of
# compiling again, so the image runs exactly what the archives ship. The
# build context is prepared by the docker job in .github/workflows/release.yml:
#
#   amd64/veredus   amd64/sidecar/
#   arm64/veredus   arm64/sidecar/
#
# Two targets: "plain" (the server alone) and "sidecar" (with the patched
# headless 0 A.D. next to it).

# The lobby's TLS trusts the system root store, so the image needs one even
# though the binary is static.
FROM alpine:3 AS plain
ARG TARGETARCH
RUN apk add --no-cache ca-certificates \
    && adduser -D -H -u 1000 veredus \
    && mkdir /data && chown veredus:veredus /data
COPY --chmod=755 ${TARGETARCH}/veredus /opt/veredus/veredus
LABEL org.opencontainers.image.source="https://github.com/victorcrimea/veredus" \
      org.opencontainers.image.description="Veredus, a dedicated server for 0 A.D." \
      org.opencontainers.image.licenses="Apache-2.0"
USER veredus
# ./config.toml and the default saves folder resolve here, so one volume
# keeps a match alive across a recreated container.
WORKDIR /data
VOLUME /data
EXPOSE 20595-20695/udp 9091/tcp
ENTRYPOINT ["/opt/veredus/veredus"]

# The sidecar is a glibc program that needs 2.36 or newer; bookworm is what
# it is built on. Its user gets a home because pyrogenesis keeps its config
# and cache there.
FROM debian:bookworm-slim AS sidecar
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 veredus \
    && mkdir /data && chown veredus:veredus /data
COPY --chmod=755 ${TARGETARCH}/veredus /opt/veredus/veredus
COPY ${TARGETARCH}/sidecar /opt/veredus/sidecar
LABEL org.opencontainers.image.source="https://github.com/victorcrimea/veredus" \
      org.opencontainers.image.description="Veredus, a dedicated server for 0 A.D., with the sidecar" \
      org.opencontainers.image.licenses="Apache-2.0 AND GPL-2.0-or-later"
USER veredus
WORKDIR /data
VOLUME /data
EXPOSE 20595-20695/udp 9091/tcp
# The flag lives in the entrypoint, not CMD, so arguments given to docker run
# are appended to it instead of dropping it.
ENTRYPOINT ["/opt/veredus/veredus", "--pyrogenesis-path", "/opt/veredus/sidecar/binaries/system/pyrogenesis"]
