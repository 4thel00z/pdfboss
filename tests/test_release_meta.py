"""Regression tests for the release automation metadata.

Guards against the release pipeline shipping a tag whose Cargo.lock still
pins the previous workspace version: from such a commit every
``cargo build/test/install --locked`` hard-fails, and unlocked builds
silently diverge from the committed lockfile.

These tests only parse committed files; they do not require the built
extension module.
"""

import json
import re
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def _workspace_version() -> str:
    manifest = tomllib.loads((REPO / "Cargo.toml").read_text())
    return manifest["workspace"]["package"]["version"]


class TestLockfileVersionSync:
    def test_lockfile_pins_current_workspace_version(self) -> None:
        """Every workspace member in Cargo.lock matches Cargo.toml.

        A mismatch is exactly the state a release tag ends up in when the
        version bump lands without a lockfile regeneration.
        """
        version = _workspace_version()
        lock = tomllib.loads((REPO / "Cargo.lock").read_text())
        members = {
            pkg["name"]: pkg["version"]
            for pkg in lock["package"]
            if pkg["name"].startswith("pdfboss")
        }
        assert members, "no pdfboss workspace members found in Cargo.lock"
        stale = {name: v for name, v in members.items() if v != version}
        assert not stale, (
            f"Cargo.lock pins {stale} but the workspace is at {version}; "
            "run `cargo update --workspace` and commit the lockfile"
        )

    def test_release_workflow_syncs_lockfile_on_release_pr(self) -> None:
        """The release workflow refreshes Cargo.lock on the release PR branch.

        release-please's generic updater only rewrites Cargo.toml; without
        this job the tagged release commit keeps the stale lockfile.
        """
        workflow = (
            REPO / ".github" / "workflows" / "release-please.yaml"
        ).read_text()
        assert "cargo update --workspace" in workflow, (
            "release-please.yaml no longer regenerates Cargo.lock on the "
            "release PR branch"
        )
        assert "prs_created" in workflow, (
            "the lockfile sync job must be gated on the release PR being "
            "created or updated"
        )
        assert "Cargo.lock" in workflow, (
            "the lockfile sync job must commit the refreshed Cargo.lock"
        )

    def test_release_please_version_marker_wiring(self) -> None:
        """Spec-pinned wiring: extra-files bumps Cargo.toml via the marker."""
        config = json.loads((REPO / "release-please-config.json").read_text())
        assert "Cargo.toml" in config["packages"]["."]["extra-files"]
        manifest_text = (REPO / "Cargo.toml").read_text()
        assert re.search(
            r'^version = "[^"]+" # x-release-please-version$',
            manifest_text,
            flags=re.MULTILINE,
        ), "Cargo.toml lost its x-release-please-version marker"
