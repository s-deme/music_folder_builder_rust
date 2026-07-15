# Fast Core/CLI/UI development image. It deliberately does not package a
# Windows desktop installer: Tauri's Windows WebView runtime must be built on Windows.
FROM node:24-bookworm-slim AS node
FROM rust:1-bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:${PATH}

# Copy only Node.js artifacts. Copying all of /usr/local would overwrite the
# Rust image's /usr/local/cargo directory.
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/ /usr/local/lib/node_modules/
RUN ln -sf /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && \
    ln -sf /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg pkg-config libdbus-1-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev librsvg2-dev libxdo-dev && \
    rm -rf /var/lib/apt/lists/*

RUN rustup component add rustfmt clippy

WORKDIR /workspace

# Dependencies are stored in named volumes by compose, not in the bind mount.
# For Linux Tauri packaging, use a dedicated CI image with WebKitGTK packages.
CMD ["bash"]
