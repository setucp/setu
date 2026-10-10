# Agent test image for scripts/e2e.sh (plan Task 11, spec §9) and
# scripts/test-install.sh (0.1.2 §3 and I2).
#
# Packages the box needs: systemd (PID 1, via e2e-init), dbus, the
# tools the script itself uses inside the box (curl and ca-certificates
# for assertions and downloads, sudo for the agent's helper path, python3
# for the 0.0.0.0 listener of `public_bind_refused_by_firewall`, iproute2
# for `ss`). Nothing here pre-installs Caddy, Postgres or nftables:
# `setu agent install` must do that inside the container.
#
# Run with `--privileged --cgroupns=private` and no cgroup bind mount:
# `e2e-init` re-mounts the container's own cgroup2 (see the file).
#
# BASE selects the image. The default is the multi-arch Ubuntu 24.04
# image, so the same file serves x86_64 and aarch64 hosts (0.1.2 I2); the
# installer matrix also uses ubuntu:22.04, debian:12 and debian:13.
ARG BASE=ubuntu:24.04
FROM ${BASE}

# WITH_SUDO=0 leaves sudo out (the installer must bring it; Debian 12 in
# the installer matrix, 0.1.2 I2d).
ARG WITH_SUDO=1
RUN apt-get update -qq \
  && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
       systemd systemd-sysv dbus curl ca-certificates python3 iproute2 \
  && if [ "$WITH_SUDO" = 1 ]; then DEBIAN_FRONTEND=noninteractive apt-get install -y -qq sudo; fi \
  && apt-get clean \
  && rm -rf /var/lib/apt/lists/*

COPY --chmod=0755 e2e-init /usr/local/sbin/e2e-init
STOPSIGNAL SIGRTMIN+3
ENTRYPOINT ["/usr/local/sbin/e2e-init"]
CMD []
