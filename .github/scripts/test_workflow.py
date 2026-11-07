"""Offline workflow-policy checks; no credentials, GitHub calls or cloud accounts."""
import ast
import itertools
from pathlib import Path
import re
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]


def gate(expression, results, selected, mode="prepare", cancelled=False):
    """Evaluate only the boolean subset used by the actual needs gates."""
    text = expression.strip().removeprefix("${{").removesuffix("}}").strip()
    text = text.replace("!cancelled()", repr(not cancelled))
    text = text.replace("&&", " and ").replace("||", " or ")
    def substitute(match):
        job, field = match.groups()
        if field == "result":
            return repr(results[job])
        key = field.removeprefix("outputs.")
        return repr(mode if key == "mode" else "true" if selected[key] else "false")
    text = re.sub(r"needs\.(check|lint|build|test)\.(result|outputs\.[a-z_]+)", substitute, text)
    tree = ast.parse(" ".join(text.splitlines()), mode="eval")
    allowed = (ast.Expression, ast.BoolOp, ast.And, ast.Or, ast.Compare, ast.Eq,
               ast.NotEq, ast.Constant, ast.UnaryOp, ast.Not)
    if any(not isinstance(node, allowed) for node in ast.walk(tree)):
        raise ValueError("Unexpected expression in dependency gate")
    def evaluate(node):
        if isinstance(node, ast.Expression):
            return evaluate(node.body)
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.BoolOp):
            values = (evaluate(value) for value in node.values)
            return all(values) if isinstance(node.op, ast.And) else any(values)
        if isinstance(node, ast.UnaryOp):
            return not evaluate(node.operand)
        if isinstance(node, ast.Compare):
            left = evaluate(node.left)
            for operator, right in zip(node.ops, node.comparators):
                right = evaluate(right)
                matches = left == right if isinstance(operator, ast.Eq) else left != right
                if not matches:
                    return False
                left = right
            return True
        raise ValueError("Unsupported dependency gate node")
    return evaluate(tree)


class WorkflowPolicy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # BaseLoader keeps GitHub's 'on' key and all scalar spellings intact.
        cls.workflow = yaml.load((ROOT / ".github/workflows/main.yml").read_text(), Loader=yaml.BaseLoader)
        cls.jobs = cls.workflow["jobs"]

    def test_six_jobs_and_independent_maintenance(self):
        self.assertEqual(set(self.jobs), {"check", "lint", "build", "test", "release", "maintenance"})
        self.assertEqual(self.workflow["name"], "main")
        self.assertEqual(set(self.workflow["on"]), {"push", "workflow_dispatch", "schedule"})
        self.assertEqual(self.jobs["lint"]["needs"], ["check"])
        self.assertEqual(self.jobs["build"]["needs"], ["check", "lint"])
        self.assertEqual(self.jobs["test"]["needs"], ["check", "lint", "build"])
        self.assertEqual(self.jobs["release"]["needs"], ["check", "lint", "build", "test"])
        self.assertNotIn("needs", self.jobs["maintenance"])
        self.assertIn("github.event_name != 'schedule'", self.jobs["check"]["if"])
        self.assertIn("github.event_name == 'schedule'", self.jobs["maintenance"]["if"])
        self.assertEqual(self.workflow["on"]["workflow_dispatch"]["inputs"]["mode"]["options"], ["prepare", "maintenance"])

    def test_selected_phases_must_succeed_and_only_empty_phases_may_skip(self):
        states = ("success", "skipped", "failure", "cancelled")
        for lint, build, tests in itertools.product((False, True), repeat=3):
            selected = dict(has_lint=lint, has_images=build, has_tests=tests, has_release=True)
            for l_result, b_result, t_result in itertools.product(states, repeat=3):
                results = dict(check="success", lint=l_result, build=b_result, test=t_result)
                l_ok = l_result == ("success" if lint else "skipped")
                b_ok = b_result == ("success" if build else "skipped")
                t_ok = t_result == ("success" if tests else "skipped")
                self.assertEqual(gate(self.jobs["build"]["if"], results, selected), build and l_ok)
                self.assertEqual(gate(self.jobs["test"]["if"], results, selected), tests and l_ok and b_ok)
                self.assertEqual(gate(self.jobs["release"]["if"], results, selected), l_ok and b_ok and t_ok)
                self.assertFalse(gate(self.jobs["release"]["if"], results, selected, mode="development"))
                self.assertFalse(gate(self.jobs["release"]["if"], results, selected, cancelled=True))
        results = dict(check="failure", lint="success", build="success", test="success")
        selected = dict(has_lint=True, has_images=True, has_tests=True, has_release=True)
        for job in ("lint", "build", "test", "release"):
            self.assertFalse(gate(self.jobs[job]["if"], results, selected))

    def test_tag_verification_and_empty_plans(self):
        results = dict(check="success", lint="success", build="skipped", test="success")
        selected = dict(has_lint=True, has_images=False, has_tests=True, has_release=True)
        self.assertFalse(gate(self.jobs["build"]["if"], results, selected, mode="publish"))
        self.assertTrue(gate(self.jobs["test"]["if"], results, selected, mode="publish"))
        self.assertTrue(gate(self.jobs["release"]["if"], results, selected, mode="publish"))
        selected = dict.fromkeys(selected, False)
        for job in ("lint", "build", "test", "release"):
            self.assertFalse(gate(self.jobs[job]["if"], results, selected))

    def test_operations_use_immutable_dedicated_actions(self):
        pin = re.compile(r"^(?:actions/(?:checkout|upload-artifact|download-artifact|create-github-app-token)|tdesaules/github-actions/(?:workflow|lint|oci|maintenance)/[a-z-]+)@[0-9a-f]{40}$")
        for job in self.jobs.values():
            self.assertNotIn("continue-on-error", job)
            for step in job["steps"]:
                self.assertNotIn("run", step)
                self.assertRegex(step["uses"], pin)

    def test_security_reports_are_advisory_and_separate_from_qualified_bytes(self):
        for name in ("test", "maintenance"):
            for step in self.jobs[name]["steps"]:
                if any(part in step["uses"] for part in ("/oci/new-sbom@", "/oci/test-vulnerabilities@", "/workflow/summarize-security@")):
                    self.assertEqual(step.get("continue-on-error"), "true")
                if "/oci/test-vulnerabilities@" in step["uses"]:
                    self.assertEqual(step["with"]["fail-on"], "none")
        for step in self.jobs["test"]["steps"]:
            if step.get("with", {}).get("name", "").startswith("qualified-"):
                self.assertIn("github.run_attempt", step["with"]["name"])
                self.assertEqual(step["with"].get("overwrite"), "false")
                paths = step["with"]["path"]
                self.assertIn("steps.qualify.outputs.archive", paths)
                self.assertIn("steps.qualify.outputs.receipt", paths)
                self.assertNotIn("extracted-root", paths)
                self.assertNotIn("security", paths)
        for word in ("sbom", "scan", "security"):
            self.assertNotIn(word, self.jobs["release"]["if"])


if __name__ == "__main__":
    unittest.main()
