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
                "windows-workspace-vcpkg-v5-${{ hashFiles(",
                "windows-workspace-vcpkg-v5-${{ hashFiles('vcpkg.json') }} # ",
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                mutated = self.workflow.replace(original, replacement, 1)
                self.assertNotEqual(mutated, self.workflow)
                self.assert_rejected(mutated, label)

    def test_setup_asset_cache_is_pr_scoped_and_main_isolated(self):
        required_fragments = (
            "      - name: Restore verified Setup assets\n",
            "windows-workspace-setup-assets-v1-${{ github.event_name == 'pull_request'",
            "format('pr-{0}', github.event.pull_request.number)",
            "windows-workspace-setup-assets-v1-trusted-main-",
            "      - name: Save verified Setup assets for this pull request\n",
            "always() && github.event_name == 'pull_request'",
            "      - name: Save verified Setup assets from trusted main\n",
            "github.event_name == 'push' && github.ref == 'refs/heads/main' && success()",
        )
        for fragment in required_fragments:
            self.assertIn(fragment, self.workflow)

        self.assertNotIn("'.github/workflows/required-ci.yml'", self.workflow)
        self.assertNotIn("'tools/windows_workspace.psm1'", self.workflow)

        mutations = {
            "PR cache written as trusted main": (
                "format('pr-{0}', github.event.pull_request.number)",
                "'trusted-main'",
            ),
            "PR cache save broadened": (
                "always() && github.event_name == 'pull_request'",
                "always()",
            ),
            "trusted main save accepts failures": (
                "github.event_name == 'push' && github.ref == 'refs/heads/main' && success()",
                "always() && github.ref == 'refs/heads/main'",
            ),
            "prepared environment cached": (
                "${{ runner.temp }}/easycon-windows-workspace/caches/assets-v1",
                "${{ runner.temp }}/easycon-windows-workspace/e",
            ),
            "runner implementation invalidates assets": (
                "${{ github.run_id }}-${{ github.run_attempt }}",
                "${{ hashFiles('.github/workflows/required-ci.yml') }}",
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                mutated = self.workflow.replace(original, replacement, 1)
                self.assertNotEqual(mutated, self.workflow)
                self.assert_rejected(mutated, label)

    def test_job_level_runner_context_is_rejected(self):
        illegal_environment = (
            "    env:\n"
            "      EASYCON_BUILD_CACHE_ROOT: "
            "${{ runner.temp }}/easycon-windows-workspace\n"
        )
        mutated = self.workflow
        if illegal_environment not in mutated:
            marker = "    timeout-minutes: 180\n"
            mutated = mutated.replace(marker, marker + illegal_environment, 1)
            self.assertNotEqual(mutated, self.workflow)
        self.assert_rejected(mutated, "runner context in job-level env")

    def test_cache_root_initialization_is_exact_and_ordered(self):
        mutations = {
            "ambient temporary root": (
                '$cacheRoot = Join-Path $env:RUNNER_TEMP "easycon-windows-workspace"',
                '$cacheRoot = Join-Path $env:TEMP "easycon-windows-workspace"',
            ),
            "wrong environment sink": (
                "Add-Content -LiteralPath $env:GITHUB_ENV",
                "Add-Content -LiteralPath $env:GITHUB_OUTPUT",
            ),
            "wrong cache environment name": (
                '"EASYCON_BUILD_CACHE_ROOT=$cacheRoot"',
                '"EASYCON_OTHER_CACHE_ROOT=$cacheRoot"',
            ),
            "Setup cache override": (
                "      - name: Set up the pinned Windows build environment\n"
                "        shell: pwsh\n",
                "      - name: Set up the pinned Windows build environment\n"
                "        env:\n"
                "          EASYCON_BUILD_CACHE_ROOT: elsewhere\n"
                "        shell: pwsh\n",
            ),
            "Workspace cache override": (
                "      - name: Run the complete Windows workspace gates\n"
                "        shell: pwsh\n"
                "        env:\n"
                "          BASE_SHA:",
                "      - name: Run the complete Windows workspace gates\n"
                "        shell: pwsh\n"
                "        env:\n"
                "          EASYCON_BUILD_CACHE_ROOT: elsewhere\n"
                "          BASE_SHA:",
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                mutated = self.workflow.replace(original, replacement, 1)
                self.assertNotEqual(mutated, self.workflow)
                self.assert_rejected(mutated, label)

        initialize_name = "      - name: Initialize the controlled cache root\n"
        restore_name = "      - name: Restore Cargo downloads\n"
        placeholder = "      - name: __CACHE_ROOT_INITIALIZATION__\n"
        reordered = self.workflow.replace(initialize_name, placeholder, 1)
        reordered = reordered.replace(restore_name, initialize_name, 1)
        reordered = reordered.replace(placeholder, restore_name, 1)
        self.assertNotEqual(reordered, self.workflow)
        self.assert_rejected(reordered, "cache root initialization after restore")

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
        cls.policy_text = (ROOT / "tools/windows_gate_policy.json").read_text(
            encoding="utf-8"
        )
        cls.policy = json.loads(cls.policy_text)

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
            '"version": 5', '"version": 5,\n  "version": 5', 1
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
            "missing file identity manifest": lambda value: value["fingerprintInputs"].remove(
                next(
                    item
                    for item in value["fingerprintInputs"]
                    if item["path"] == "crates/easycon-file-identity/Cargo.toml"
                )
            ),
            "fingerprint Windows case alias": lambda value: value["fingerprintInputs"][-1].update(path="TOOLS/WINDOWS_BUILD_ENVIRONMENT.JSON"),
            "fingerprint dot component": lambda value: value["fingerprintInputs"][-1].update(path="tools/./provision_vision_test_model.py"),
            "uppercase scripts commit": lambda value: value["vcpkg"].update(scriptsCommit="C" * 40),
            "registry divergence": lambda value: value["vcpkg"].update(registryBaseline="0" * 40),
            "uppercase SHA-512": lambda value: value["vcpkg"]["internalTools"][0].update(sha512="A" * 128),
            "uppercase 7zip executable SHA-256": lambda value: value["vcpkg"][
                "internalTools"
            ][2].update(executableSha256="A" * 64),
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


class WindowsWorkspaceModuleContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = (ROOT / "tools/windows_workspace.psm1").read_text(
            encoding="utf-8"
        )

    def assert_rejected(self, module, label):
        self.assertTrue(
            GUARD.windows_workspace_module_failures(module),
            "{} mutation must be rejected".format(label),
        )

    def test_current_workspace_vcpkg_projection_is_guarded(self):
        self.assertEqual(GUARD.windows_workspace_module_failures(self.module), [])

    def test_workspace_vcpkg_projection_mutations_are_rejected(self):
        mutations = {
            "missing root binding": (
                '"set(Z_VCPKG_ROOT_DIR',
                '"set(EASYCON_UNUSED_ROOT',
            ),
            "non-internal root binding": (
                'CACHE INTERNAL `"EasyCon workspace-local vcpkg applocal root',
                'CACHE PATH `"EasyCon workspace-local vcpkg applocal root',
            ),
            "same-hash reuse": ("-ReplaceExisting", ""),
            "in-place wrapper write": (
                "Write-EasyConUtf8FileAtomically -Path $wrapper",
                "[System.IO.File]::WriteAllText($wrapper",
            ),
            "non-overwrite publish": (
                "[System.IO.File]::Move($temporary, $destinationPath, $true)",
                "[System.IO.File]::Move($temporary, $destinationPath, $false)",
            ),
            "non-unique temporary": (
                "[System.IO.FileMode]::CreateNew",
                "[System.IO.FileMode]::OpenOrCreate",
            ),
            "broadened root": (
                'Allowed = @(".vcpkg-root", "scripts", "vcpkg.exe")',
                'Allowed = @(".vcpkg-root", "scripts", "vcpkg.exe", "other")',
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                self.assertIn(original, self.module)
                mutated = self.module.replace(original, replacement, 1)
                self.assertNotEqual(mutated, self.module)
                self.assert_rejected(mutated, label)
        self.assert_rejected(
            self.module + "\n$env:VCPKG_APPLOCAL_DEPS = '0'\n",
            "disabled applocal",
        )

    def test_prepared_tree_single_scan_mutations_are_rejected(self):
        mutations = {
            "missing controlled enumeration": (
                "Directory.EnumerateFileSystemEntries",
                "Directory.GetFiles",
            ),
            "nonincremental hash": (
                "IncrementalHash.CreateHash",
                "SHA256.Create",
            ),
            "Verify Cargo fallback fingerprint": (
                "Get-EasyConPreparedTreeVerification -Path $preparedPaths.cargoVendor",
                "Get-EasyConTreeFingerprint -Path $preparedPaths.cargoVendor",
            ),
            "assertion bypass": (
                "Get-EasyConPreparedTreeVerification -Path $Path",
                "Get-EasyConLegacyPreparedTreeVerification -Path $Path",
            ),
            "physical boundary bypass": (
                "PreparedTreeAuditor]::ValidatePhysicalTree",
                "PreparedTreeAuditor]::LegacyPhysicalTree",
            ),
            "Verify vcpkg full-tree binding root": (
                "-VcpkgExecutable $tools.vcpkg `\n        -TrustedRoot $location.EnvironmentRoot",
                "-VcpkgExecutable $tools.vcpkg `\n        -TrustedRoot $location.CacheRoot",
            ),
            "vcpkg full-tree binding": (
                "PreparedTreeAuditor]::BindVcpkgCheckout(",
                "PreparedTreeAuditor]::AuditTree(",
            ),
            "vcpkg binding lifetime": (
                "$binding.Dispose()",
                "$binding.ReleaseBeforeGit()",
            ),
            "vcpkg critical path identity revalidation": (
                "vcpkg checkout binding path",
                "vcpkg checkout held handle only",
            ),
            "vcpkg extended Win32 CreateFile path": (
                "GetWin32ExtendedPath(logicalPath)",
                "logicalPath",
            ),
            "vcpkg minimal read-share entry binding": (
                "uint desiredAccess = FileReadData | FileListDirectory;",
                "uint desiredAccess = 0;",
            ),
            "content digest ordinal pre-sort": (
                "private static bool ScanDirectory(\n"
                "            string directory,\n"
                "            string tree,\n"
                "            string[] labels,\n"
                "            string[] references,\n"
                "            bool auditContent,\n"
                "            PreparedTreeAuditResult result,\n"
                "            List<TreeFileRecord> files)\n"
                "        {\n"
                "            List<string> entries = new List<string>();",
                "private static bool ScanDirectory(\n"
                "            string directory,\n"
                "            string tree,\n"
                "            string[] labels,\n"
                "            string[] references,\n"
                "            bool auditContent,\n"
                "            PreparedTreeAuditResult result,\n"
                "            List<TreeFileRecord> files)\n"
                "        {\n"
                "            List<string> entries = new List<string>();\n"
                "            entries.Sort(StringComparer.OrdinalIgnoreCase);",
            ),
            "vcpkg binding pre-handle attributes": (
                "PreparedVcpkgBoundPhysicalEntry bound = OpenBoundPhysicalEntry(\n"
                "                    entry,\n"
                "                    null,",
                "File.GetAttributes(entry);\n"
                "                PreparedVcpkgBoundPhysicalEntry bound = "
                "OpenBoundPhysicalEntry(\n"
                "                    entry,\n"
                "                    null,",
            ),
            "vcpkg basic entry audit": (
                "GetBoundBasicInformation(",
                "GetBoundFileInformation(",
            ),
            "vcpkg basic metric detached from OS query": (
                "result.PhysicalBasicInformationQueries++;\n"
                "            if (!GetFileInformationByHandleEx(",
                "if (!GetFileInformationByHandleEx(",
            ),
            "vcpkg extended UNC normalization": (
                'return @"\\\\?\\UNC\\" + logicalPath.Substring(2);',
                'return logicalPath;',
            ),
            "vcpkg final path logical normalization": (
                "return NormalizeFullPath(buffer.ToString());",
                "return buffer.ToString();",
            ),
        }
        for label, (original, replacement) in mutations.items():
            with self.subTest(label=label):
                self.assertIn(original, self.module)
                mutated = self.module.replace(original, replacement)
                self.assertNotEqual(mutated, self.module)
                self.assert_rejected(mutated, label)


class WindowsGatePolicyContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.environment_module = (ROOT / "tools/windows_workspace.psm1").read_text(
            encoding="utf-8"
        )
        cls.policy_module = (ROOT / "tools/windows_gate_policy.ps1").read_text(
            encoding="utf-8"
        )
        cls.runner = (ROOT / "tools/run_windows_workspace.ps1").read_text(
            encoding="utf-8"
        )
        cls.policy_text = (ROOT / "tools/windows_gate_policy.json").read_text(
            encoding="utf-8"
        )
        cls.policy = json.loads(cls.policy_text)

    def assert_policy_rejected(self, value, label):
        text = value if isinstance(value, str) else json.dumps(value)
        with self.assertRaises(ValueError, msg="{} must be rejected".format(label)):
            GUARD.parse_windows_gate_policy(text)

    def assert_module_rejected(self, policy_module, runner, label):
        self.assertTrue(
            GUARD.windows_gate_policy_failures(
                self.environment_module, policy_module, runner
            ),
            "{} mutation must be rejected".format(label),
        )

    def test_current_policy_is_valid_and_environment_identity_is_separate(self):
        self.assertEqual(GUARD.parse_windows_gate_policy(self.policy_text), self.policy)
        self.assertEqual(
            GUARD.windows_gate_policy_failures(
                self.environment_module, self.policy_module, self.runner
            ),
            [],
        )
        self.assertNotIn(
            "tools/run_windows_workspace.ps1",
            GUARD.expected_windows_environment_fingerprint_paths(),
        )
        self.assertIn(
            "crates/easycon-file-identity/Cargo.toml",
            GUARD.expected_windows_environment_fingerprint_paths(),
        )

    def test_policy_schema_order_case_and_jobs_are_strict(self):
        mutations = {
            "duplicate root key": self.policy_text.replace(
                '"version": 1', '"version": 1, "version": 1', 1
            ),
            "unknown root key": dict(self.policy, unknown=True),
            "noninteger jobs": dict(self.policy, cargoJobs=True),
            "wrong jobs": dict(self.policy, cargoJobs=3),
            "reordered gates": dict(self.policy, gates=list(reversed(self.policy["gates"]))),
            "path case alias": self.policy_text.replace(
                "tools/validate_specs.py", "TOOLS/VALIDATE_SPECS.PY", 1
            ),
        }
        for label, mutation in mutations.items():
            with self.subTest(label=label):
                self.assert_policy_rejected(mutation, label)

    def test_gate_policy_bypass_jobs_and_evidence_mutations_are_rejected(self):
        mutations = {
            "public GateInvoker": (
                self.policy_module.replace(
                    "[ValidateRange(0, 7200000)]\n        [int]$LeaseTimeoutMilliseconds",
                    "[scriptblock]$GateInvoker,\n\n        [ValidateRange(0, 7200000)]\n        [int]$LeaseTimeoutMilliseconds",
                    1,
                ),
                self.runner,
            ),
            "Targeted jobs override": (
                self.policy_module.replace(
                    'StartsWith("--jobs=")', 'StartsWith("--worker-jobs=")', 1
                ),
                self.runner,
            ),
            "overwrite evidence": (
                self.policy_module.replace(
                    "[System.IO.File]::Move($temporary, $destination)",
                    "[System.IO.File]::Move($temporary, $destination, $true)",
                    1,
                ),
                self.runner,
            ),
            "missing post-policy timing check": (
                self.policy_module.replace(
                    '"after the final gate"', '"after an unrelated boundary"'
                ),
                self.runner,
            ),
            "case-insensitive runner": (
                self.policy_module,
                self.runner.replace("IgnoreCase = $false", "IgnoreCase = $true", 1),
            ),
        }
        for label, (policy_module, runner) in mutations.items():
            with self.subTest(label=label):
                self.assert_module_rejected(policy_module, runner, label)

    def test_policy_snapshot_import_handoff_and_capture_recheck_are_required(self):
        mutations = {
            "policy source earliest AST capture": (
                self.environment_module,
                self.policy_module.replace(
                    "$MyInvocation.MyCommand.ScriptBlock",
                    "$null",
                    1,
                ),
                self.runner,
            ),
            "capture-time physical recheck": (
                self.environment_module,
                self.policy_module.replace(
                    "changed during snapshot capture",
                    "changed after a later boundary",
                    1,
                ),
                self.runner,
            ),
            "runner private snapshot handoff": (
                self.environment_module,
                self.policy_module,
                self.runner.replace(
                    "Set-EasyConGatePolicyRunnerSnapshot",
                    "Apply-EasyConGateRunnerSource",
                    1,
                ),
            ),
        }
        for label, (environment_module, policy_module, runner) in mutations.items():
            with self.subTest(label=label):
                self.assertTrue(
                    GUARD.windows_gate_policy_failures(
                        environment_module, policy_module, runner
                    ),
                    "{} mutation must be rejected".format(label),
                )


if __name__ == "__main__":
    unittest.main(verbosity=2)
