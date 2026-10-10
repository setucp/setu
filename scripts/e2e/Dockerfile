# Agent test image for scripts/e2e.sh (plan Task 11, spec §9).
#
# Only the tools the script itself needs inside the box: curl and
# ca-certificates (assertions, downloads), sudo (the agent's helper
# path), python3 (the 0.0.0.0 listener of `public_bind_refused_by_firewall`)
# and iproute2 (`ss`). Nothing here pre-installs Caddy, Postgres or
# nftables: `setu agent install` must do that inside the container.
#
# Run with `--privileged --cgroupns=private` and no cgroup bind mount:
# `e2e-init` re-mounts the container's own cgroup2 (see the file).
#
# BASE selects the systemd image (0.1.2 §3: scripts/test-install.sh runs the
# installer on Ubuntu 22.04/24.04 and Debian 12/13). systemd-sysv is the
# Debian packaging of PID 1 (a no-op on Ubuntu).
ARG BASE=jrei/systemd-ubuntu:24.04
FROM ${BASE}

RUN apt-get update -qq \
  && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
       curl ca-certificates sudo python3 iproute2 systemd-sysv \
  && apt-get clean \
  && rm -rf /var/lib/apt/lists/*

COPY --chmod=0755 e2e-init /usr/local/sbin/e2e-init
STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/usr/local/sbin/e2e-init"]
CMD []
