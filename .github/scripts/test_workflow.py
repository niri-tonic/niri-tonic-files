"""Offline workflow-policy checks; no credentials, GitHub calls or cloud accounts."""
import ast
import itertools
from pathlib import Path
import re
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]


def workflow_value(expression, context):
    """Evaluate the limited checkout/routing expression subset in fixtures."""
    text = expression.strip().removeprefix("${{").removesuffix("}}").strip()
    for key in sorted(context, key=len, reverse=True):
        text = text.replace(key, repr(context[key]))
    text = text.replace("&&", " and ").replace("||", " or ")
    tree = ast.parse(text, mode="eval")
    functions = {"startsWith": lambda text, prefix: text.startswith(prefix),
                 "format": lambda pattern, value: pattern.format(value)}
    allowed = (ast.Expression, ast.BoolOp, ast.And, ast.Or, ast.Compare, ast.Eq,
               ast.Constant, ast.Call, ast.Name, ast.Load)
    for node in ast.walk(tree):
        if not isinstance(node, allowed):
            raise ValueError("Unexpected routing expression")
        if isinstance(node, ast.Name) and node.id not in functions:
            raise ValueError("Unknown routing function")
    return eval(compile(tree, "<workflow fixture>", "eval"), {"__builtins__": {}}, functions)


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
        self.assertEqual(self.workflow["on"]["workflow_dispatch"]["inputs"]["mode"]["options"], ["prepare", "publish", "maintenance"])

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

    def test_release_checkouts_preserve_annotated_tags(self):
        # checkout's implicit event SHA makes its full-history fallback fetch the
        # peeled commit into refs/tags/<version>, losing the release annotation.
        # Tag readers need an explicit ref; branch runs must keep the event SHA.
        contexts = (
            (dict(github_event_name="push", github_ref="refs/tags/1.0.0", mode="", tag=""), "refs/tags/1.0.0"),
            (dict(github_event_name="push", github_ref="refs/heads/main", mode="", tag=""), "event-sha"),
            (dict(github_event_name="workflow_dispatch", github_ref="refs/heads/main", mode="prepare", tag="1.0.0"), "event-sha"),
            (dict(github_event_name="workflow_dispatch", github_ref="refs/heads/main", mode="publish", tag="1.0.0"), "refs/tags/1.0.0"),
        )
        for job in ("check", "test", "release"):
            checkout = self.jobs[job]["steps"][0]
            self.assertTrue(checkout["uses"].startswith("actions/checkout@"))
            self.assertEqual(checkout["with"]["fetch-depth"], "0")
            for case, expected in contexts:
                context = {
                    "github.event_name": case["github_event_name"],
                    "github.ref": case["github_ref"], "github.sha": "event-sha",
                    "inputs.mode": case["mode"], "inputs.release-tag": case["tag"],
                    "needs.check.outputs.mode": "publish" if expected.startswith("refs/tags/") else "prepare",
                    "needs.check.outputs.version": "1.0.0",
                }
                self.assertEqual(workflow_value(checkout["with"]["ref"], context), expected)

    def test_manual_publication_reuses_tag_plan_only_from_main(self):
        planner = next(step for step in self.jobs["check"]["steps"] if step.get("id") == "plan")
        self.assertEqual(planner["with"]["source-sha"], "${{ steps.checkout.outputs.commit }}")
        for branch, expected in (("main", "push"), ("other", "workflow_dispatch")):
            context = {"github.event_name": "workflow_dispatch", "github.ref": "refs/heads/" + branch,
                       "inputs.mode": "publish", "inputs.release-tag": "1.0.0"}
            self.assertEqual(workflow_value(planner["with"]["event"], context), expected)
            self.assertEqual(workflow_value(planner["with"]["ref"], context), "refs/tags/1.0.0")
        for job in ("test", "release"):
            for step in self.jobs[job]["steps"]:
                if "/read-binary-release@" in step["uses"] or "/publish-binary-release@" in step["uses"]:
                    self.assertEqual(step["with"]["tag"], "${{ needs.check.outputs.version }}")
        # Publication must never acquire the preparation App token or create a tag.
        for step in self.jobs["release"]["steps"]:
            if "create-github-app-token@" in step["uses"] or "/prepare-binary-release@" in step["uses"]:
                self.assertEqual(step["if"], "needs.check.outputs.mode == 'prepare'")

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
