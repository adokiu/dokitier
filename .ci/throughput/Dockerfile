# Minimal image for the tunnel throughput bench: our core binary is mounted
# from the runner's target dir, only the networking/iperf tooling lives here.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates iproute2 iputils-ping iperf3 procps kmod \
    && rm -rf /var/lib/apt/lists/*
CMD ["sleep", "infinity"]
