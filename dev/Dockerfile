# Development container: cligen and clixon at the same revisions as the
# Yocto recipes (meta-ethernet-switch-os, recipes-clixon), plus Rust.
# Built and run by dev/container.sh.
FROM rust:1.94.1-bookworm

ARG CLIGEN_REV=e43397cd5c951de85a22620122c02078a05babcd
ARG CLIXON_REV=62a901b1c6215703a7c37e1ff4d51a155587af7d

RUN apt-get update \
    && apt-get install -y --no-install-recommends bison flex libssl-dev iproute2 jq procps \
    && rm -rf /var/lib/apt/lists/*

RUN git clone https://github.com/clicon/cligen.git /tmp/cligen \
    && git -C /tmp/cligen checkout -q "$CLIGEN_REV" \
    && cd /tmp/cligen && ./configure && make -j"$(nproc)" && make install \
    && git clone https://github.com/clicon/clixon.git /tmp/clixon \
    && git -C /tmp/clixon checkout -q "$CLIXON_REV" \
    && cd /tmp/clixon && ./configure --with-restconf=native --disable-nghttp2 \
    && make -j"$(nproc)" && make install && ldconfig \
    && rm -rf /tmp/cligen /tmp/clixon

# Socket group, and the user clixon_restconf drops to (clixon.xml).
# rust-lld, the default linker for x86_64 Linux, does not search /usr/local/lib.
ENV CLIXON_LIB_DIR=/usr/local/lib

RUN groupadd --system clicon \
    && useradd --system --gid clicon --no-create-home --shell /bin/false clicon

RUN rustup component add clippy rustfmt
