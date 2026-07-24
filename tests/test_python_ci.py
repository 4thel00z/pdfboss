"""Pins the python-ci workflow contract the async binding tests rely on."""

from pathlib import Path

import yaml

WORKFLOW = (
    Path(__file__).parent.parent / ".github" / "workflows" / "python-ci.yml"
)


def test_pytest_job_installs_pytest_asyncio() -> None:
    workflow = yaml.safe_load(WORKFLOW.read_text())
    steps = workflow["jobs"]["pytest"]["steps"]
    install = next(
        step for step in steps if step.get("name", "").startswith("Install package")
    )
    assert "pytest-asyncio" in install["run"]
