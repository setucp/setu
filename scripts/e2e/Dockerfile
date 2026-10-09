# Agent test image for scripts/e2e.sh (Task 11).
#
# The script prefers `docker run jrei/systemd-ubuntu:24.04` directly and
# installs the few missing essentials (`sudo curl ca-certificates`) with
# apt after boot — this file exists so CI (or a host without network at
# `docker run` time) can pre-bake them instead:
#
#   docker build -t setu-e2e-agent scripts/e2e
#
# and then point the script at it with `E2E_IMAGE=setu-e2e-agent`.
FROM jrei/systemd-ubuntu:24.04

RUN apt-get update -qq \
  && apt-get install -y -qq sudo curl ca-certificates \
  && apt-get clean \
  && rm -rf /var/lib/apt/lists/*
