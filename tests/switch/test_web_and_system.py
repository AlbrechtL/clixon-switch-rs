"""The status page clixon_restconf serves from www/, and system state."""

import pytest
import requests

from lib import paths


@pytest.fixture
def web(switch):
    """The web root, which is the RESTCONF root without /restconf."""
    return switch.restconf.base.removesuffix("/restconf")


@pytest.mark.parametrize(
    "path, content_type",
    [("/", "text/html"), ("/app.js", "application/javascript")],
)
def test_serves_the_web_ui(web, path, content_type):
    response = requests.get(web + path, timeout=10)
    assert response.status_code == 200
    assert response.headers["content-type"] == content_type


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
