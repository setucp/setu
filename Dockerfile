# Control-plane image: the musl-static `setu` binary on scratch.
# Needs SETU_DATABASE_URL, SETU_SECRET_KEY, SETU_PUBLIC_URL at runtime
# (see docs/RELEASING.md); migrations run on boot under an advisory lock.
FROM scratch AS binary
COPY setu /setu
EXPOSE 7000
USER 65532:65532
ENTRYPOINT ["/setu"]
CMD ["serve"]
