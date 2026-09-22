"""http-data serving and system state."""

import pytest
import requests

from lib import paths


@pytest.fixture
def web(switch):
    """The web root, which is the RESTCONF root without /restconf."""
    return switch.restconf.base.removesuffix("/restconf")


# This repository installs no pages, only the empty http-data root. That the
# root exists is what makes this a 404 rather than an internal error:
# clixon's http_data_check_file_path() resolves the root before the path.
def test_serves_no_file_outside_the_web_root(web):
    assert requests.get(web + "/clixon.xml", timeout=10).status_code == 404


def test_system_state(switch):
    state = switch.restconf.data(paths.SYSTEM)["state"]
    assert state["hostname"] == switch.kernel.hostname()
    assert int(state["uptime"]) > 0
    assert int(state["memory-total"]) >= int(state["memory-available"])


def test_interface_state_data(switch):
    state = switch.restconf.data(f"{paths.interface('lan1')}/state")
    assert state["admin-status"] == "UP"
    assert "in-octets" in state["counters"]
