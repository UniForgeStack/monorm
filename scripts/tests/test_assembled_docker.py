import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import assembled_docker as docker  # noqa: E402


class AssembledDockerTest(unittest.TestCase):
    def test_the_image_name_gives_the_runtime(self):
        for image, runtime in [("python:3.12-alpine", "python"), ("node:22-alpine", "node"), ("oven/bun:alpine", "bun")]:
            self.assertEqual(docker.runtime_of(image), runtime)
        with self.assertRaises(SystemExit):
            docker.runtime_of("alpine:3.20")

    def test_only_runtime_commands_run_in_the_container(self):
        for command in ["python", "node", "bun", "/w/venv/bin/python", "C:/v/Scripts/python.exe"]:
            self.assertTrue(docker.in_container(command), command)
        for command in ["npm", "/usr/bin/uv", "cargo"]:
            self.assertFalse(docker.in_container(command), command)

    def test_the_command_line_mounts_the_repository_and_passes_the_environment(self):
        line = docker.wrap("node:22-alpine", ["node", "use.mjs"], Path("/r"), Path("/r/target/x"), {"MONORM_DATABASE_URL": "sqlite://:memory:"})
        self.assertEqual(line[:2], ["docker", "run"])
        self.assertIn("host", line)
        self.assertEqual(line[line.index("-v") + 1], "/r:/r")
        self.assertEqual(line[line.index("-w") + 1], "/r/target/x")
        self.assertIn("MONORM_DATABASE_URL=sqlite://:memory:", line)
        self.assertEqual(line[-3:], ["node:22-alpine", "node", "use.mjs"])


if __name__ == "__main__":
    unittest.main()
