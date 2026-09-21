"""One switch, seen through RESTCONF and through its kernel.

The views below return typed values rather than the strings the old shell
suite compared, so a failing assert prints a real difference.
"""

import time

from lib.restconf import Restconf
from lib.shell import Shell, run_json

BRIDGE = "br-lan"

CONFIG = "/usr/local/etc/clixon.xml"
XMLDB = "/usr/local/var/run/clixon-switch"
PERSISTENT = "/usr/local/var/lib/clixon/clixon-switch"
FACTORY_DEFAULT = "/usr/local/share/clixon-switch/factory-default.xml"
PREPARE_DATASTORE = "/usr/local/lib/clixon-switch/prepare-datastore"


def _one(value, default=None):
    """mstpctl and ip print either an object or a one-element array."""
    if isinstance(value, list):
        return value[0] if value else default
    return value if value is not None else default


class KernelView:
    """What the kernel says, via ip(8), bridge(8) and sysfs."""

    def __init__(self, shell: Shell):
        self.sh = shell

    def link(self, name: str) -> dict | None:
        return _one(run_json(self.sh, ["ip", "-j", "link", "show", name]))

    def link_detail(self, name: str) -> dict:
        """The link's type-specific settings (linkinfo.info_data)."""
        link = _one(run_json(self.sh, ["ip", "-j", "-d", "link", "show", name]), {})
        return link.get("linkinfo", {}).get("info_data", {})

    def exists(self, name: str) -> bool:
        return self.sh.succeeds(["ip", "link", "show", name])

    def master(self, name: str) -> str | None:
        return (self.link(name) or {}).get("master")

    def is_up(self, name: str) -> bool:
        return "UP" in (self.link(name) or {}).get("flags", [])

    def vlans(self, port: str) -> dict[int, set[str]]:
        """VLAN id -> flags, e.g. {1: {"PVID", "Egress Untagged"}}."""
        entry = _one(run_json(self.sh, ["bridge", "-j", "vlan", "show", "dev", port]), {})
        return {v["vlan"]: set(v.get("flags") or []) for v in entry.get("vlans", [])}

    def self_vlans(self) -> list[int]:
        """The VLANs the bridge itself is a member of, i.e. routed VLANs."""
        entries = run_json(self.sh, ["bridge", "-j", "vlan", "show", "dev", BRIDGE], default=[])
        return sorted(
            v["vlan"]
            for e in entries
            if e.get("ifname") == BRIDGE
            for v in e.get("vlans", [])
        )

    def addresses(self, name: str, *, dynamic: bool | None = None) -> list[str]:
        entry = _one(
            run_json(self.sh, ["ip", "-j", "addr", "show", "dev", name], default=[]), {}
        )
        return [
            f"{a['local']}/{a['prefixlen']}"
            for a in entry.get("addr_info", [])
            if a.get("family") == "inet"
            and (dynamic is None or bool(a.get("dynamic")) == dynamic)
        ]

    def default_routes(self) -> list[tuple[str, str]]:
        routes = run_json(self.sh, ["ip", "-j", "route", "show", "default"], default=[])
        return [(r["gateway"], r["dev"]) for r in routes]

    def vlan_msti(self, vlan: int) -> int | None:
        """Which MST instance the bridge maps this VLAN to."""
        entry = _one(
            run_json(self.sh, ["bridge", "-j", "vlan", "global", "show", "dev", BRIDGE]), {}
        )
        for v in entry.get("vlans", []):
            if v["vlan"] <= vlan <= v.get("vlanEnd", v["vlan"]):
                return v.get("msti")
        return None

    def msti_state(self, port: str, msti: int) -> str | None:
        """The port's kernel forwarding state in one MST instance."""
        entry = _one(run_json(self.sh, ["bridge", "-j", "mst", "show", "dev", port]), {})
        for m in entry.get("mst", []):
            if m.get("msti") == msti:
                return m.get("state")
        return None

    def mac(self, name: str) -> str:
        return self.sh.run(["cat", f"/sys/class/net/{name}/address"]).strip()

    def ifindex(self, name: str) -> int:
        return int(self.sh.run(["cat", f"/sys/class/net/{name}/ifindex"]).strip())

    def hostname(self) -> str:
        return self.sh.run(["cat", "/proc/sys/kernel/hostname"]).strip()


class MstpdView:
    """mstpd's own view, via mstpctl -f json."""

    def __init__(self, shell: Shell):
        self.sh = shell

    def _json(self, *args):
        return run_json(self.sh, ["mstpctl", "-f", "json", *args])

    def bridge(self) -> dict:
        return _one(self._json("showbridge", BRIDGE), {})

    def ports(self) -> list:
        return self._json("showport", BRIDGE) or []

    def port(self, name: str) -> dict:
        return _one(self._json("showportdetail", BRIDGE, name), {})

    def tree_port(self, port: str, msti: int) -> dict:
        return _one(self._json("showtreeport", BRIDGE, port, str(msti)), {})

    def msti_state(self, port: str, msti: int) -> str:
        """The port's state in one MST instance, in the kernel's words."""
        state = self.tree_port(port, msti).get("state")
        return "blocking" if state == "discarding" else state

    def mstis(self) -> list[int]:
        return _one(self._json("showmstilist", BRIDGE), {}).get("mstids", [])

    def region(self) -> tuple[str, int]:
        info = _one(self._json("showmstconfid", BRIDGE), {})
        return info.get("configuration-name"), info.get("revision-level")


class ProcView:
    """Running processes by name.

    Zombies do not count: whatever started the suite may not reap the
    daemons it kills, and a process that has exited is not running.
    """

    def __init__(self, shell: Shell):
        self.sh = shell

    def pids(self, name: str) -> list[int]:
        out = self.sh.run(["ps", "-eo", "pid=,stat=,comm="], check=False)
        pids = []
        for line in out.splitlines():
            fields = line.split(None, 2)
            if len(fields) == 3 and fields[2] == name and not fields[1].startswith("Z"):
                pids.append(int(fields[0]))
        return pids

    def count(self, name: str) -> int:
        return len(self.pids(name))


class FileView:
    def __init__(self, shell: Shell):
        self.sh = shell

    def read(self, path: str) -> str:
        return self.sh.run(["cat", path], check=False)

    def write(self, path: str, text: str) -> None:
        self.sh.run(["sh", "-c", f"printf %s {_quote(text)} > {_quote(path)}"])

    def mode(self, path: str) -> str:
        return self.sh.run(["stat", "-c", "%a", path]).strip()

    def ls(self, path: str) -> list[str]:
        return sorted(self.sh.run(["ls", path]).split())

    def is_symlink(self, path: str) -> bool:
        return self.sh.succeeds(["test", "-L", path])


def _quote(text: str) -> str:
    return "'" + text.replace("'", "'\\''") + "'"


class SnmpView:
    """net-snmp's client tools, as the read-only user the tests configure.

    MIBS= keeps the tools from loading MIB files, so every OID is numeric,
    and the client keeps its own persistent directory away from snmpd's.
    """

    PERSISTENT_DIR = "/tmp/snmp-client"
    ENV = ["env", "MIBS=", f"SNMP_PERSISTENT_DIR={PERSISTENT_DIR}"]

    def __init__(self, shell: Shell, host: str = "127.0.0.1"):
        self.sh = shell
        self.host = host
        self.user = "nms"
        self.auth = "authpass123"
        self.priv = "privpass123"
        self._prepared = False

    def prepare(self) -> None:
        if not self._prepared:
            self.sh.run(["mkdir", "-p", f"{self.PERSISTENT_DIR}/cert_indexes"])
            self._prepared = True

    def _credentials(self, user=None, level="authPriv", auth=None, priv=None):
        argv = ["-v3", "-l", level, "-u", user or self.user]
        if level != "noAuthNoPriv":
            argv += ["-a", "SHA", "-A", auth or self.auth]
        if level == "authPriv":
            argv += ["-x", "AES", "-X", priv or self.priv]
        return argv

    def run(self, command: str, *args: str, options=(), **credentials) -> str:
        self.prepare()
        argv = [
            *self.ENV,
            f"snmp{command}",
            *self._credentials(**credentials),
            "-On",
            "-Oqv",
            "-Ox",
            *options,
            "-t",
            "2",
            "-r",
            "0",
            self.host,
            *args,
        ]
        return self.sh.run(argv, check=False).strip()

    def get(self, oid: str, *, text: bool = False, **credentials) -> str:
        """One value. `text` prints octet strings as text (-Oa)."""
        return self.run("get", oid, options=("-Oa",) if text else (), **credentials)

    def walk(self, oid: str, **credentials) -> str:
        return self.run("walk", oid, **credentials)

    def set(self, oid: str, type_: str, value: str, **credentials) -> str:
        return self.run("set", oid, type_, value, **credentials)


class Node:
    """A switch: its RESTCONF API and its kernel."""

    def __init__(self, name: str, shell: Shell, restconf: Restconf):
        self.name = name
        self.sh = shell
        self.restconf = restconf
        self.kernel = KernelView(shell)
        self.mstpd = MstpdView(shell)
        self.procs = ProcView(shell)
        self.files = FileView(shell)
        self.snmp = SnmpView(shell)

    def __repr__(self) -> str:
        return f"<Node {self.name} {self.restconf.base}>"

    def wait_restconf(self, timeout: float = 30) -> None:
        deadline = time.monotonic() + timeout
        while True:
            try:
                if self.restconf.get("data/openconfig-interfaces:interfaces").status == 200:
                    return
            except Exception:  # noqa: BLE001 - the daemon may not be listening yet
                pass
            if time.monotonic() >= deadline:
                raise AssertionError(f"{self.name}: RESTCONF did not come back")
            time.sleep(0.1)

    def restart_backend(self) -> None:
        """What the init script does: a cold backend on the startup database."""
        self.sh.run(["pkill", "-x", "clixon_backend"], check=False)
        while self.procs.count("clixon_backend"):
            time.sleep(0.05)
        self.sh.run([PREPARE_DATASTORE, XMLDB, PERSISTENT, FACTORY_DEFAULT])
        self.sh.run(
            [
                "sh",
                "-c",
                f"clixon_backend -F -f {CONFIG} -l e >>/tmp/clixon_backend.log 2>&1 &",
            ]
        )
        self.wait_restconf()
