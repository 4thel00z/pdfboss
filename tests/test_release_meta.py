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

import yaml

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


class TestPdfbossFontsReleasePipeline:
    """The pdfboss-fonts data package gets its own release-please component
    and Trusted-Publishing job, independent of the pdfboss engine release.
    """

    def test_pdfboss_fonts_package_registered_for_release(self) -> None:
        config = json.loads((REPO / "release-please-config.json").read_text())
        assert "packages/pdfboss-fonts" in config["packages"]
        pkg = config["packages"]["packages/pdfboss-fonts"]
        assert pkg["package-name"] == "pdfboss-fonts"
        # The root package's tagging scheme must be untouched.
        assert config["packages"]["."]["package-name"] == "pdfboss"
        assert config["packages"]["."]["include-component-in-tag"] is False
        manifest = json.loads((REPO / ".release-please-manifest.json").read_text())
        assert "packages/pdfboss-fonts" in manifest

    def test_full_extra_requires_fonts_package(self) -> None:
        root = tomllib.loads((REPO / "pyproject.toml").read_text())
        full = root["project"]["optional-dependencies"]["full"]
        assert any(dep.startswith("pdfboss-fonts") for dep in full)

    def test_release_workflow_publishes_fonts_package(self) -> None:
        """The pdfboss-fonts publish job is correctly wired for Trusted
        Publishing, independent of (and not fooled by) the pre-existing
        ``publish-pypi`` job for the root ``pdfboss`` engine package, which
        also references ``gh-action-pypi-publish`` and also builds a
        `pdfboss-fonts`-mentioning comment elsewhere in the file.
        """
        workflow_path = REPO / ".github" / "workflows" / "release-please.yaml"
        workflow = yaml.safe_load(workflow_path.read_text())

        jobs = workflow["jobs"]
        fonts_job_name = "publish-pdfboss-fonts"
        assert fonts_job_name in jobs, (
            f"expected a `{fonts_job_name}` job publishing the fonts package"
        )
        job = jobs[fonts_job_name]

        # Gated on the fonts-component release output, NOT the root
        # `release_created` output -- otherwise this job would fire on
        # every pdfboss engine release, even when pdfboss-fonts didn't
        # change.
        condition = job.get("if", "")
        assert "fonts_release_created" in condition, (
            "publish-pdfboss-fonts must be gated on the fonts-specific "
            f"release output, got if: {condition!r}"
        )
        assert "release-please.outputs.release_created" not in condition, (
            "publish-pdfboss-fonts must not be gated on the root "
            f"release_created output, got if: {condition!r}"
        )

        # Trusted Publishing requires the OIDC id-token permission.
        permissions = job.get("permissions", {})
        assert permissions.get("id-token") == "write", (
            "publish-pdfboss-fonts is missing `permissions: id-token: write` "
            "required for PyPI Trusted Publishing"
        )

        steps = job.get("steps", [])
        uses_list = [step.get("uses", "") for step in steps]
        assert any("pypa/gh-action-pypi-publish" in u for u in uses_list), (
            "publish-pdfboss-fonts must publish via pypa/gh-action-pypi-publish"
        )

        # The fonts package is pure Python: it must be built with
        # `python -m build`, not maturin (which is for the compiled
        # pdfboss engine wheel).
        job_text = yaml.dump(job)
        assert "maturin" not in job_text, (
            "publish-pdfboss-fonts must not use maturin; pdfboss-fonts is "
            "a pure-Python package built with `python -m build`"
        )
        run_steps = " ".join(step.get("run", "") for step in steps)
        assert "python -m build" in run_steps or any(
            "build" in u for u in uses_list
        ), (
            "publish-pdfboss-fonts must build the fonts sdist/wheel with "
            "`python -m build` (or an equivalent `build` step)"
        )
        assert "packages/pdfboss-fonts" in run_steps, (
            "the build step must target the packages/pdfboss-fonts directory"
        )
