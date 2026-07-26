#!/usr/bin/env python3
"""Regression contracts for Windows Setup, Workspace, and pinned inputs."""

import copy
import importlib.util
import json
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
GUARD_PATH = ROOT / "tools" / "check_repository_guards.py"
SPEC = importlib.util.spec_from_file_location("check_repository_guards", GUARD_PATH)
GUARD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUARD)


class RequiredCiContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (ROOT / ".github/workflows/required-ci.yml").read_text(
            encoding="utf-8"
        )

    def assert_rejected(self, workflow, label):
        self.assertTrue(
            GUARD.required_ci_failures(workflow), "{} must be rejected".format(label)
        )

    def test_current_workflow_is_valid(self):
        self.assertEqual(GUARD.required_ci_failures(self.workflow), [])

    def test_setup_and_workspace_are_separate_active_steps(self):
        mutations = {
            "commented Setup": (
                "./tools/run_windows_workspace.ps1 -Mode Setup",
                "# ./tools/run_windows_workspace.ps1 -Mode Setup",
            ),
            "Setup provisions through Workspace": (
                "./tools/run_windows_workspace.ps1 -Mode Setup",
                "./tools/run_windows_workspace.ps1 -Mode Workspace",
            ),
            "commented Workspace": (
                "./tools/run_windows_workspace.ps1 -Mode Workspace -RequireCleanTree",
                "# ./tools/run_windows_workspace.ps1 -Mode Workspace -RequireCleanTree",
            ),
            "legacy provision flag": (
                "./tools/run_windows_workspace.ps1 -Mode Workspace -RequireCleanTree",
                "./tools/run_windows_workspace.ps1 -Mode Workspace -Provision -RequireCleanTree",
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                mutated = self.workflow.replace(original, replacement, 1)
                self.assertNotEqual(mutated, self.workflow)
                self.assert_rejected(mutated, label)

    def test_disabled_workspace_runner_is_rejected(self):
        marker = "      - name: Run the complete Windows workspace gates\n"
        mutated = self.workflow.replace(marker, marker + "        if: false\n", 1)
        self.assert_rejected(mutated, "disabled Workspace")

    def test_cache_is_speed_only_and_trusted_on_save(self):
        mutations = {
            "untrusted save": (
                "github.event_name == 'push' && github.ref == 'refs/heads/main' && "
                "steps.cargo_cache.outputs.cache-hit != 'true'",
                "always()",
            ),
            "broad restore": (
                "${{ runner.temp }}/easycon-windows-workspace/caches/vcpkg-binary-cache",
                "${{ github.workspace }}",
            ),
            "incomplete key": (
                "windows-workspace-vcpkg-v4-${{ hashFiles(",
                "windows-workspace-vcpkg-v4-${{ hashFiles('vcpkg.json') }} # ",
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                mutated = self.workflow.replace(original, replacement, 1)
                self.assertNotEqual(mutated, self.workflow)
                self.assert_rejected(mutated, label)

    def test_yaml_and_actions_are_fail_closed(self):
        duplicate = self.workflow.replace(
            "permissions:\n  contents: read",
            "permissions:\n  contents: read\n  contents: write",
            1,
        )
        self.assert_rejected(duplicate, "duplicate YAML key")
        unpinned = self.workflow.replace(
            "actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830",
            "actions/cache/restore@v4",
            1,
        )
        self.assert_rejected(unpinned, "unpinned action")

    def test_policy_commands_remain_exact_active_invocations(self):
        for label, original, replacement in (
            (
                "printed policy command",
                "python tools/validate_specs.py",
                'Write-Output "python tools/validate_specs.py"',
            ),
            (
                "unreachable policy command",
                "python tools/check_markdown_links.py",
                "if ($false) { python tools/check_markdown_links.py }",
            ),
        ):
            with self.subTest(label=label):
                self.assert_rejected(
                    self.workflow.replace(original, replacement, 1), label
                )


class WindowsBuildEnvironmentContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.configuration_text = (
            ROOT / "tools/windows_build_environment.json"
        ).read_text(encoding="utf-8")
        cls.configuration = json.loads(cls.configuration_text)

    def assert_rejected(self, value, label):
        text = value if isinstance(value, str) else json.dumps(value)
        with self.assertRaises(ValueError, msg="{} must be rejected".format(label)):
            GUARD.parse_windows_build_environment(text)

    def test_current_configuration_is_valid(self):
        self.assertEqual(
            GUARD.parse_windows_build_environment(self.configuration_text),
            self.configuration,
        )

    def test_duplicate_json_keys_are_rejected(self):
        duplicate_root = self.configuration_text.replace(
            '"version": 3', '"version": 3,\n  "version": 3', 1
        )
        self.assert_rejected(duplicate_root, "duplicate root key")
        duplicate_tool = self.configuration_text.replace(
            '"name": "cmake"', '"name": "cmake",\n        "name": "cmake"', 1
        )
        self.assert_rejected(duplicate_tool, "duplicate internal tool key")

    def test_exact_key_sets_are_required(self):
        mutations = []
        for path in (
            "root",
            "fingerprint",
            "host",
            "vcpkg",
            "asset",
            "tools",
            "internal",
            "native",
        ):
            mutated = copy.deepcopy(self.configuration)
            if path == "root":
                mutated["unknown"] = 1
            elif path == "fingerprint":
                mutated["fingerprintInputs"][0]["unknown"] = 1
            elif path == "host":
                mutated["hostTools"]["unknown"] = 1
            elif path == "vcpkg":
                mutated["vcpkg"]["unknown"] = 1
            elif path == "asset":
                mutated["vcpkg"]["windowsAsset"]["unknown"] = 1
            elif path == "tools":
                mutated["vcpkg"]["toolsManifest"]["unknown"] = 1
            elif path == "internal":
                mutated["vcpkg"]["internalTools"][0]["unknown"] = 1
            else:
                mutated["vcpkg"]["nativeDependencies"][0]["unknown"] = 1
            mutations.append((path, mutated))
        for label, mutated in mutations:
            with self.subTest(label=label):
                self.assert_rejected(mutated, "unknown {} key".format(label))

    def test_types_versions_hashes_and_sets_are_strict(self):
        mutations = {}
        for label, mutate in {
            "string schema": lambda value: value.update(version="3"),
            "boolean bytes": lambda value: value["vcpkg"]["windowsAsset"].update(bytes=True),
            "old schema": lambda value: value.update(version=1),
            "unfrozen target": lambda value: value.update(target="x86_64-unknown-linux-gnu"),
            "fingerprint order": lambda value: value["fingerprintInputs"].reverse(),
            "fingerprint kind": lambda value: value["fingerprintInputs"][0].update(kind="auto"),
            "uppercase scripts commit": lambda value: value["vcpkg"].update(scriptsCommit="C" * 40),
            "registry divergence": lambda value: value["vcpkg"].update(registryBaseline="0" * 40),
            "uppercase SHA-512": lambda value: value["vcpkg"]["internalTools"][0].update(sha512="A" * 128),
            "duplicate tool": lambda value: value["vcpkg"]["internalTools"].__setitem__(2, copy.deepcopy(value["vcpkg"]["internalTools"][0])),
            "negative port version": lambda value: value["vcpkg"]["nativeDependencies"][0].update(portVersion=-1),
            "unknown native dependency": lambda value: value["vcpkg"]["nativeDependencies"][0].update(name="other"),
        }.items():
            mutated = copy.deepcopy(self.configuration)
            mutate(mutated)
            mutations[label] = mutated
        for label, mutated in mutations.items():
            with self.subTest(label=label):
                self.assert_rejected(mutated, label)

    def test_urls_must_be_canonical_https(self):
        for label, mutation in {
            "vcpkg userinfo": "https://user@github.com/microsoft/vcpkg.git",
            "tool query": self.configuration["vcpkg"]["internalTools"][0]["url"] + "?latest=1",
            "tool HTTP": self.configuration["vcpkg"]["internalTools"][0]["url"].replace("https://", "http://"),
        }.items():
            with self.subTest(label=label):
                mutated = copy.deepcopy(self.configuration)
                if label.startswith("vcpkg"):
                    mutated["vcpkg"]["scriptsRepository"] = mutation
                else:
                    mutated["vcpkg"]["internalTools"][0]["url"] = mutation
                self.assert_rejected(mutated, label)

    def test_setup_summary_carries_the_controlled_vcpkg_executable(self):
        module = (ROOT / "tools/windows_workspace.psm1").read_text(encoding="utf-8")
        self.assertIn(
            "vcpkgExecutable = $vcpkg.Executable",
            module,
            "Setup must carry the audited vcpkg executable into the stamp tool records",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
