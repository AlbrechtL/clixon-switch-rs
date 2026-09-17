# Development container: cligen and clixon at the same revisions as the
# Yocto recipes (meta-ethernet-switch-os, recipes-clixon), mstpd with the
# layer's patches, plus Rust.
# Built and run by dev/container.sh.
FROM rust:1.94.1-bookworm

ARG CLIGEN_REV=e43397cd5c951de85a22620122c02078a05babcd
ARG CLIXON_REV=62a901b1c6215703a7c37e1ff4d51a155587af7d

# iproute2 from backports: bookworm's 6.1 shows neither mst_enabled nor
# "bridge mst".
RUN echo "deb http://deb.debian.org/debian bookworm-backports main" \
        > /etc/apt/sources.list.d/backports.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends -t bookworm-backports iproute2 \
    && apt-get install -y --no-install-recommends bison flex libssl-dev jq procps \
       busybox udhcpc autoconf automake \
    && rm -rf /var/lib/apt/lists/*

RUN git clone https://github.com/clicon/cligen.git /tmp/cligen \
    && git -C /tmp/cligen checkout -q "$CLIGEN_REV" \
    && cd /tmp/cligen && ./configure && make -j"$(nproc)" && make install \
    && git clone https://github.com/clicon/clixon.git /tmp/clixon \
    && git -C /tmp/clixon checkout -q "$CLIXON_REV" \
    && cd /tmp/clixon && ./configure --with-restconf=native --disable-nghttp2 \
    && make -j"$(nproc)" && make install && ldconfig \
    && rm -rf /tmp/cligen /tmp/clixon

# mstpd at the version of meta-openembedded, with the patches of
# meta-ethernet-switch-os (recipes-networking/mstpd).
ARG MSTPD_REV=8995e5d11ee89f2251734e34cea711bb5a5bb911
ARG LAYER_REV=master
RUN git clone https://github.com/mstpd/mstpd.git /tmp/mstpd \
    && git -C /tmp/mstpd checkout -q "$MSTPD_REV" \
    && git clone -q https://github.com/AlbrechtL/meta-ethernet-switch-os.git /tmp/layer \
    && git -C /tmp/layer checkout -q "$LAYER_REV" \
    && git -C /tmp/mstpd apply /tmp/layer/recipes-networking/mstpd/files/*.patch \
    && cd /tmp/mstpd && ./autogen.sh && ./configure --prefix=/usr \
    && make -j"$(nproc)" && make install \
    && rm -rf /tmp/mstpd /tmp/layer

# Socket group, and the user clixon_restconf drops to (clixon.xml).
# rust-lld, the default linker for x86_64 Linux, does not search /usr/local/lib.
ENV CLIXON_LIB_DIR=/usr/local/lib

RUN groupadd --system clicon \
    && useradd --system --gid clicon --no-create-home --shell /bin/false clicon

RUN rustup component add clippy rustfmt
