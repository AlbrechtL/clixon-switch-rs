import pytest

# So that assertions inside the helpers print a difference, not just "False".
pytest.register_assert_rewrite("lib")


def pytest_addoption(parser):
    parser.addoption(
        "--restconf",
        default="http://localhost:8080/restconf",
        help="RESTCONF root of the switch under test (single-node suite)",
    )
    parser.addoption(
        "--container",
        default=None,
        help="run kernel commands in this container instead of locally",
    )
