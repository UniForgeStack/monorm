"""Runs the runtime commands of `check-assembled-consumers.py --docker <image>` inside a container."""
import os
from pathlib import Path

RUNTIMES = ("python", "node", "bun")


def runtime_of(image):
    """`python`, `node` or `bun`: the runtime that the image provides, from its repository name."""
    name = image.rsplit("/", 1)[-1].split(":")[0]
    if name not in RUNTIMES:
        raise SystemExit(f"--docker needs a python, node or bun image, got {image}")
    return name


def in_container(executable):
    """A runtime command runs in the container; npm, uv and the other host tools stay on the host."""
    return Path(str(executable)).stem in RUNTIMES


def wrap(image, command, root, cwd=None, env=None):
    """The `docker run` command line for `command`: the repository is mounted at the same path, so paths need no translation."""
    wrapped = ["docker", "run", "--rm", "--network", "host", "-v", f"{root}:{root}", "-w", str(cwd or root), "-e", "HOME=/tmp"]
    if hasattr(os, "getuid"):
        wrapped += ["--user", f"{os.getuid()}:{os.getgid()}"]
    for key, value in (env or {}).items():
        wrapped += ["-e", f"{key}={value}"]
    return [*wrapped, image, *map(str, command)]
