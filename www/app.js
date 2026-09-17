// Switch status page. Everything comes from RESTCONF on the same origin
// (clixon_restconf serves this file too). Read-only for now: configuration
// changes would add a restconf(method, path, body) next to restconfGet.

"use strict";

const RESTCONF = "/restconf/data/";
const REFRESH_MS = 5000;
// swupdate's web server on the same host. Plain HTTP: swupdate is built
// without TLS.
const UPDATE_PORT = 8080;

class RestconfError extends Error {
  constructor(status, body) {
    const error = [].concat(body?.["ietf-restconf:errors"]?.error ?? [])[0];
    super(error?.["error-message"] ?? `HTTP ${status}`);
    this.status = status;
    this.tag = error?.["error-tag"];
  }
}

// GET a RESTCONF data resource as RFC 7951 JSON: the value of its single
// top-level member. With optional, a resource that does not exist is null.
async function restconfGet(path, { optional = false } = {}) {
  const response = await fetch(RESTCONF + path, {
    headers: { Accept: "application/yang-data+json" },
    cache: "no-store",
  });
  const text = await response.text();
  const body = text ? JSON.parse(text) : {};
  if (!response.ok) {
    const error = new RestconfError(response.status, body);
    if (optional && (response.status === 404 || error.tag === "invalid-value")) {
      return null;
    }
    throw error;
  }
  return Object.values(body)[0] ?? null;
}

// ---------------------------------------------------------------------------
// DOM helpers. Data is only ever inserted as text.

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else node.setAttribute(key, value);
  }
  for (const child of children.flat()) {
    if (child !== null && child !== undefined) {
      node.append(child instanceof Node ? child : String(child));
    }
  }
  return node;
}

function facts(entries) {
  return entries
    .filter(([, value]) => value !== null && value !== undefined && value !== "")
    .flatMap(([label, value]) => [el("dt", {}, label), el("dd", {}, value)]);
}

function badge(status) {
  const text = status ?? "UNKNOWN";
  return el("span", { class: text === "UP" ? "badge up" : "badge" }, text);
}

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------------------
// Formatting

function duration(seconds) {
  if (seconds === undefined) return null;
  let s = Number(seconds);
  const days = Math.floor(s / 86400);
  s %= 86400;
  const parts = [];
  if (days) parts.push(`${days} d`);
  parts.push(`${Math.floor(s / 3600)} h`, `${Math.floor((s % 3600) / 60)} min`);
  return parts.join(" ");
}

function bytes(value) {
  if (value === undefined) return "";
  let n = Number(value);
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${n.toFixed(1)} ${units[i]}`;
}

function datetime(value) {
  if (!value) return null;
  const date = new Date(value);
  return isNaN(date) ? value : date.toLocaleString();
}

const list = (value) => [].concat(value ?? []);

// ---------------------------------------------------------------------------
// Rendering

function renderSystem(system) {
  const s = system?.state ?? {};
  const firmware = [s["os-name"], s["os-version"]].filter(Boolean).join(" ");
  $("hostname").textContent = s.hostname || "Switch";
  $("firmware").textContent = firmware;
  document.title = `${s.hostname || "Switch"} – status`;

  let memory = null;
  if (s["memory-total"] && s["memory-available"]) {
    const total = Number(s["memory-total"]);
    const used = total - Number(s["memory-available"]);
    const percent = Math.round((used / total) * 100);
    memory = el(
      "div",
      {},
      `${bytes(used * 1024)} of ${bytes(total * 1024)} used (${percent} %)`,
      el("div", { class: "meter" }, el("span", { style: `width:${percent}%` })),
    );
  }
  const load = ["1", "5", "15"].map((m) => s[`load-average-${m}`]);
  $("system").replaceChildren(
    ...facts([
      ["Host name", s.hostname],
      ["Firmware", firmware],
      ["Kernel", s["kernel-release"]],
      ["Uptime", duration(s.uptime)],
      ["Switch clock", datetime(s["current-datetime"])],
      ["Load", load.every((l) => l !== undefined) ? load.join(" / ") : null],
      ["Memory", memory],
    ]),
  );
}

function renderManagement(interfaces) {
  const svis = interfaces.filter((i) => i["openconfig-vlan:routed-vlan"]);
  if (svis.length === 0) {
    $("management").replaceChildren(el("p", { class: "muted" }, "No routed VLAN interface."));
    return;
  }
  $("management").replaceChildren(
    ...svis.map((svi) => {
      const routed = svi["openconfig-vlan:routed-vlan"];
      const ipv4 = routed["openconfig-if-ip:ipv4"] ?? {};
      const addresses = list(ipv4.addresses?.address).map((a) => {
        const state = a.state ?? a.config ?? {};
        const origin = state.origin ? ` (${state.origin.toLowerCase()})` : "";
        return `${a.ip}/${state["prefix-length"]}${origin}`;
      });
      const lease = ipv4.state?.["clixon-switch:dhcp-lease"];
      const dhcp = ipv4.state?.["dhcp-client"] ?? ipv4.config?.["dhcp-client"];
      return el(
        "div",
        { class: "svi" },
        el("h3", {}, svi.name, badge(svi.state?.["oper-status"])),
        el(
          "dl",
          { class: "facts" },
          facts([
            ["VLAN", routed.config?.vlan],
            ["IPv4", addresses.length ? addresses.join(", ") : "none"],
            ["DHCP client", dhcp ? (lease ? "bound" : "waiting for a lease") : "off"],
            ["Gateway", list(lease?.router)[0]],
            ["DNS", list(lease?.["dns-server"]).join(", ")],
            ["Domain", lease?.domain],
            ["Lease expires in", duration(lease?.["remaining-time"])],
          ]),
        ),
      );
    }),
  );
}

function portVlan(port, mode, groups) {
  if (mode === "PORT_BASED") {
    const group = groups.find((g) => list(g.config?.port ?? g.state?.port).includes(port.name));
    if (!group) return "–";
    const name = group.config?.name ?? group.state?.name;
    return `group ${group.id}${name ? ` (${name})` : ""}`;
  }
  const config =
    port["openconfig-if-ethernet:ethernet"]?.["openconfig-vlan:switched-vlan"]?.config;
  if (!config) return "–";
  if (config["interface-mode"] === "TRUNK") {
    const trunk = list(config["trunk-vlans"]);
    const native = config["native-vlan"] !== undefined ? `native ${config["native-vlan"]}, ` : "";
    return `trunk: ${native}${trunk.length ? trunk.join(", ") : "all"}`;
  }
  return `access ${config["access-vlan"] ?? 1}`;
}

function renderPorts(interfaces, mode, groups) {
  const ports = interfaces.filter((i) => i["openconfig-if-ethernet:ethernet"]);
  const rows = ports.map((port) => {
    const state = port.state ?? {};
    const counters = state.counters ?? {};
    const enabled = port.config?.enabled ?? true;
    return el(
      "tr",
      {},
      el("td", {}, el("strong", {}, port.name)),
      el("td", {}, enabled ? badge(state["oper-status"]) : el("span", { class: "badge" }, "DISABLED")),
      el("td", {}, portVlan(port, mode, groups)),
      el("td", { class: "wrap" }, port.config?.description ?? ""),
      el("td", { class: "num" }, bytes(counters["in-octets"])),
      el("td", { class: "num" }, bytes(counters["out-octets"])),
      el("td", { class: "mono" }, port["openconfig-if-ethernet:ethernet"].state?.["hw-mac-address"] ?? ""),
    );
  });
  $("ports").replaceChildren(
    ...(rows.length ? rows : [el("tr", {}, el("td", { colspan: 7, class: "muted" }, "No ports."))]),
  );
}

function renderVlans(mode, vlans, groups) {
  const head = (...names) => el("tr", {}, names.map((n) => el("th", {}, n)));
  let rows;
  if (mode === "PORT_BASED") {
    $("vlans-title").textContent = "Port-based VLAN groups";
    $("vlans-head").replaceChildren(head("Group", "Name", "Ports"));
    rows = groups.map((g) =>
      el(
        "tr",
        {},
        el("td", {}, g.id),
        el("td", {}, g.state?.name ?? g.config?.name ?? ""),
        el("td", { class: "wrap" }, list(g.state?.port ?? g.config?.port).join(", ")),
      ),
    );
  } else {
    $("vlans-title").textContent = "VLANs (802.1Q)";
    $("vlans-head").replaceChildren(head("VLAN", "Name", "Status", "Ports"));
    rows = vlans.map((v) => {
      const state = v.state ?? v.config ?? {};
      const members = list(v.members?.member).map((m) => m.state?.interface);
      return el(
        "tr",
        {},
        el("td", {}, v["vlan-id"]),
        el("td", {}, state.name ?? ""),
        el("td", {}, badge(state.status === "ACTIVE" ? "UP" : state.status ?? "ACTIVE")),
        el("td", { class: "wrap" }, members.join(", ")),
      );
    });
  }
  $("vlans").replaceChildren(
    ...(rows.length ? rows : [el("tr", {}, el("td", { colspan: 4, class: "muted" }, "None."))]),
  );
}

// ---------------------------------------------------------------------------

let loading = false;

async function refresh() {
  if (loading) return;
  loading = true;
  $("status").textContent = "Updating…";
  try {
    const [system, interfaces, sw] = await Promise.all([
      restconfGet("clixon-switch:system"),
      restconfGet("openconfig-interfaces:interfaces", { optional: true }),
      restconfGet("clixon-switch:switch", { optional: true }),
    ]);
    const mode = sw?.state?.["vlan-mode"] ?? sw?.config?.["vlan-mode"] ?? "DOT1Q";
    const [vlans, groups] = await Promise.all([
      mode === "DOT1Q" ? restconfGet("clixon-switch:vlans", { optional: true }) : null,
      mode === "PORT_BASED"
        ? restconfGet("clixon-switch:port-based-vlans", { optional: true })
        : null,
    ]);
    const ifaces = list(interfaces?.interface);
    const groupList = list(groups?.group);
    renderSystem(system);
    renderManagement(ifaces);
    renderPorts(ifaces, mode, groupList);
    renderVlans(mode, list(vlans?.vlan), groupList);
    $("error").hidden = true;
    $("status").textContent = `Updated ${new Date().toLocaleTimeString()}`;
  } catch (error) {
    $("error").textContent = `Cannot read the switch's state: ${error.message}`;
    $("error").hidden = false;
    $("status").textContent = "";
  } finally {
    loading = false;
  }
}

$("host").textContent = location.hostname;
$("update").href = `http://${location.hostname}:${UPDATE_PORT}/`;
$("refresh").addEventListener("click", refresh);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) refresh();
});
setInterval(() => {
  if (!document.hidden) refresh();
}, REFRESH_MS);
refresh();
