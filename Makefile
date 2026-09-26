# Installs the plugin with its YANG, clixon configuration and factory
# default. The plugin is built by cargo; PLUGIN points at the library.
# Before Linux 7.1, which has the bridge's stp_mode, the kernel runs
# /sbin/bridge-stp to be told to leave spanning tree to mstpd. It lives
# outside PREFIX: link it to LIBDIR/clixon-switch/bridge-stp.
#
#   cargo build --release
#   make install DESTDIR=... PREFIX=/usr SYSCONFDIR=/etc LOCALSTATEDIR=/var
#
# The Yocto recipe and the dev container both install through this.

APP = clixon-switch

PREFIX ?= /usr/local
SYSCONFDIR ?= $(PREFIX)/etc
LIBDIR ?= $(PREFIX)/lib
DATADIR ?= $(PREFIX)/share
LOCALSTATEDIR ?= $(PREFIX)/var
RUNSTATEDIR ?= $(LOCALSTATEDIR)/run
RESTCONF_PORT ?= 80

# Where clixon_restconf serves static files from (http-data). This repository
# ships no pages; it only creates the directory, because clixon resolves the
# root before the request path and a missing one makes every static-file
# request fail outright instead of answering 404. A distro that has a web UI
# overrides this and owns the directory itself.
HTTP_DATA_ROOT ?= $(DATADIR)/$(APP)/www

# Factory default: front ports and management address.
LAN_PORTS ?= lan1 lan2 lan3 lan4 lan5 lan6 lan7 lan8
LAN_ADDRESS ?= 192.168.1.1/24

PLUGIN ?= target/release/libclixon_switch_plugin.so
BUILDDIR ?= build

INSTALL ?= install

.PHONY: all install install-mibs clean

all: $(BUILDDIR)/clixon.xml $(BUILDDIR)/factory-default.xml

$(BUILDDIR)/clixon.xml: clixon/clixon.xml.in Makefile
	mkdir -p $(BUILDDIR)
	sed -e 's|@SYSCONFDIR@|$(SYSCONFDIR)|g' \
	    -e 's|@LIBDIR@|$(LIBDIR)|g' \
	    -e 's|@DATADIR@|$(DATADIR)|g' \
	    -e 's|@LOCALSTATEDIR@|$(LOCALSTATEDIR)|g' \
	    -e 's|@RUNSTATEDIR@|$(RUNSTATEDIR)|g' \
	    -e 's|@RESTCONF_PORT@|$(RESTCONF_PORT)|g' \
	    -e 's|@HTTP_DATA_ROOT@|$(HTTP_DATA_ROOT)|g' \
	    $< > $@

$(BUILDDIR)/factory-default.xml: scripts/factory-default.sh Makefile
	mkdir -p $(BUILDDIR)
	sh $< "$(LAN_PORTS)" "$(LAN_ADDRESS)" > $@

install: all
	$(INSTALL) -D -m 0644 $(BUILDDIR)/clixon.xml $(DESTDIR)$(SYSCONFDIR)/clixon.xml
	$(INSTALL) -D -m 0644 clixon/autocli.xml $(DESTDIR)$(SYSCONFDIR)/clixon/$(APP)/autocli.xml
	$(INSTALL) -D -m 0644 clixon/$(APP)_cli.cli $(DESTDIR)$(LIBDIR)/$(APP)/clispec/$(APP)_cli.cli
	$(INSTALL) -D -m 0644 $(PLUGIN) $(DESTDIR)$(LIBDIR)/$(APP)/backend/$(APP)_backend.so
	$(INSTALL) -D -m 0755 scripts/prepare-datastore.sh $(DESTDIR)$(LIBDIR)/$(APP)/prepare-datastore
	$(INSTALL) -D -m 0755 scripts/udhcpc-script.sh $(DESTDIR)$(LIBDIR)/$(APP)/udhcpc-script
	$(INSTALL) -D -m 0755 scripts/bridge-stp.sh $(DESTDIR)$(LIBDIR)/$(APP)/bridge-stp
	$(INSTALL) -D -m 0644 $(BUILDDIR)/factory-default.xml $(DESTDIR)$(DATADIR)/$(APP)/factory-default.xml
	$(INSTALL) -d $(DESTDIR)$(HTTP_DATA_ROOT)
	cd yang && find . -name '*.yang' | sort | while read -r f; do \
	    $(INSTALL) -D -m 0644 "$$f" "$(DESTDIR)$(DATADIR)/$(APP)/yang/$$f" || exit 1; \
	done
	$(INSTALL) -d $(DESTDIR)$(LOCALSTATEDIR)/lib/clixon/$(APP)

# The SMIv2 MIBs the SNMP agent serves, for loading into an NMS. Not part of
# install: the agent does not parse MIBs, they are published with the
# firmware instead.
install-mibs:
	$(INSTALL) -D -m 0644 mib/MANIFEST $(DESTDIR)$(DATADIR)/$(APP)/mib/MANIFEST
	for f in mib/*.txt; do \
	    $(INSTALL) -m 0644 "$$f" "$(DESTDIR)$(DATADIR)/$(APP)/mib/" || exit 1; \
	done

clean:
	rm -rf $(BUILDDIR)
