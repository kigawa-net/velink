# Build from the repository root:
# docker build -f crates/velink-remote/Dockerfile -t velink-remote:dev .
FROM rust:1.85-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
COPY apps ./apps
RUN cargo build --release -p velink-remote

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    xvfb tigervnc-scraping-server x11-utils xfce4 dbus-x11 xterm fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 velink
COPY --from=build /src/target/release/velink-remote /usr/local/bin/velink-remote
# Debian's TigerVNC binary name differs from upstream.
RUN if [ ! -x /usr/bin/x0vncserver ]; then ln -s /usr/bin/x0tigervncserver /usr/local/bin/x0vncserver; fi
USER 10001:10001
ENV HOME=/home/velink
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/velink-remote"]
