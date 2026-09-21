"""The single switch the dev container starts, restored between tests.

`dev/in-container.sh` has already installed the plugin, created the ports and
started clixon on the factory default. The suite runs inside that container by
default; `--container` points it at another one instead.
"""

import pytest

from lib import paths
from lib.node import PERSISTENT, Node
from lib.restconf import Restconf
from lib.shell import DockerShell, LocalShell

CONFIG_SNAPSHOT = "data?content=config"

# The plugin's own words when the kernel keeps spanning tree for itself.
NO_USER_STP = "did not leave spanning tree to mstpd"


@pytest.fixture(scope="session")
def switch(request) -> Node:
    container = request.config.getoption("--container")
    shell = DockerShell(container) if container else LocalShell("switch")
    node = Node("switch", shell, Restconf(request.config.getoption("--restconf")))
    try:
        node.wait_restconf(timeout=5)
    except AssertionError:
        pytest.skip(
            f"no RESTCONF at {node.restconf.base}; run this suite with "
            f"dev/container.sh, which starts clixon first"
        )
    return node


@pytest.fixture(scope="session")
def factory_state(switch) -> tuple[dict, str]:
    """The configuration and startup database the container started with."""
    response = switch.restconf.get(CONFIG_SNAPSHOT)
    assert response.status == 200, f"reading the factory default: {response!r}"
    return response.json(), switch.files.read(f"{PERSISTENT}/startup_db")


@pytest.fixture(scope="session")
def spanning_tree(switch):
    """Skips the test unless the kernel leaves spanning tree to mstpd.

    It does so for a bridge in a container only from Linux 7.1, which added
    the bridge's stp_mode; before that it asks /sbin/bridge-stp, and only
    for bridges in the host's network namespace.
    """
    body = paths.wrap_stp({"global": {"config": {"enabled-protocol": [paths.RSTP]}}})
    response = switch.restconf.patch(paths.DATA, body)
    if response.status >= 400:
        if NO_USER_STP in response.text:
            release = switch.sh.run(["uname", "-r"]).strip()
            pytest.skip(
                f"the kernel does not leave spanning tree to mstpd: needs the "
                f"bridge's stp_mode (Linux 7.1), running {release}"
            )
        pytest.fail(f"enabling spanning tree failed unexpectedly: {response!r}")
    switch.restconf.delete(paths.STP)
    return True


@pytest.fixture(autouse=True)
def clean_switch(request, switch, factory_state):
    """Puts the factory default back *before* each test.

    Restoring in setup rather than teardown leaves a failed test's state in
    place, so the container can be inspected afterwards.
    """
    config, startup_db = factory_state
    if request.node.get_closest_marker("restart"):
        # These tests corrupt or rewrite the startup database, and want a
        # backend that has never seen the running configuration.
        switch.files.write(f"{PERSISTENT}/startup_db", startup_db)
        switch.restart_backend()
    else:
        response = switch.restconf.put("data", config)
        assert response.status in (200, 201, 204), f"restoring the factory default: {response!r}"
    yield
