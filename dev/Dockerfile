# Development container: cligen and clixon at the same revisions as the
# Yocto recipes (meta-ethernet-switch-os, recipes-clixon), clixon and mstpd
# with the layer's patches, net-snmp and libsmi's smidump, plus Rust.
# The layer is the build context "layer" (dev/container.sh passes it).
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
       libsnmp-dev snmpd snmp \
    && rm -rf /var/lib/apt/lists/*

COPY --from=layer recipes-clixon/clixon/files/ /tmp/clixon-patches/
RUN git clone https://github.com/clicon/cligen.git /tmp/cligen \
    && git -C /tmp/cligen checkout -q "$CLIGEN_REV" \
    && cd /tmp/cligen && ./configure && make -j"$(nproc)" && make install \
    && git clone https://github.com/clicon/clixon.git /tmp/clixon \
    && git -C /tmp/clixon checkout -q "$CLIXON_REV" \
    && git -C /tmp/clixon apply /tmp/clixon-patches/*.patch \
    && cd /tmp/clixon && echo 7.8.0 > .version \
    && ./configure --with-restconf=native --disable-nghttp2 --enable-netsnmp \
    && make -j"$(nproc)" && make install && ldconfig \
    && rm -rf /tmp/cligen /tmp/clixon /tmp/clixon-patches

# mstpd at the version of meta-openembedded, with the patches of
# meta-ethernet-switch-os (recipes-networking/mstpd).
ARG MSTPD_REV=8995e5d11ee89f2251734e34cea711bb5a5bb911
COPY --from=layer recipes-networking/mstpd/files/ /tmp/mstpd-patches/
RUN git clone https://github.com/mstpd/mstpd.git /tmp/mstpd \
    && git -C /tmp/mstpd checkout -q "$MSTPD_REV" \
    && git -C /tmp/mstpd apply /tmp/mstpd-patches/*.patch \
    && cd /tmp/mstpd && ./autogen.sh && ./configure --prefix=/usr \
    && make -j"$(nproc)" && make install \
    && rm -rf /tmp/mstpd /tmp/mstpd-patches

# smidump turns MIBs into YANG (scripts/mib-to-yang.sh). The tarball of the
# meta-openembedded recipe: Debian's libsmi has no IETF MIBs (not DFSG-free).
ARG LIBSMI_SHA256=f21accdadb1bb328ea3f8a13fc34d715baac6e2db66065898346322c725754d3
RUN curl -fsSL -o /tmp/libsmi.tar.gz \
        https://www.ibr.cs.tu-bs.de/projects/libsmi/download/libsmi-0.5.0.tar.gz \
    && echo "$LIBSMI_SHA256  /tmp/libsmi.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/libsmi.tar.gz -C /tmp \
    && cd /tmp/libsmi-0.5.0 && ./configure --prefix=/usr/local \
    && make -j"$(nproc)" && make install && ldconfig \
    && cp -r /tmp/libsmi-0.5.0/mibs /usr/local/share/libsmi-mibs \
    && rm -rf /tmp/libsmi.tar.gz /tmp/libsmi-0.5.0

# Socket group, and the user clixon_restconf drops to (clixon.xml).
# rust-lld, the default linker for x86_64 Linux, does not search /usr/local/lib.
ENV CLIXON_LIB_DIR=/usr/local/lib

RUN groupadd --system clicon \
    && useradd --system --gid clicon --no-create-home --shell /bin/false clicon

RUN rustup component add clippy rustfmt
