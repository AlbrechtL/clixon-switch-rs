# Lab image: the dev image (dev/Dockerfile) with the plugin built and
# installed at image build time, so that every switch container starts
# without compiling. lab/up.sh builds the dev image first.
FROM clixon-switch-dev

COPY . /src
WORKDIR /src
RUN cargo build --locked --release -p clixon-switch-plugin \
    && make -s install BUILDDIR=/tmp/build RESTCONF_PORT=8080 \
        PLUGIN=target/release/libclixon_switch_plugin.so \
    && install -D -m 0755 lab/entrypoint.sh /lab/entrypoint.sh \
    && install -D -m 0755 lab/startup-config.sh /lab/startup-config.sh \
    && install -D -m 0755 scripts/factory-default.sh /lab/factory-default.sh \
    && rm -rf target /usr/local/cargo/registry

ENTRYPOINT ["/lab/entrypoint.sh"]
