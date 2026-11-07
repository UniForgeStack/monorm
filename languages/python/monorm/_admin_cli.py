"""``python -m monorm admin``: serves the admin of this project.

The settings come from the ``[admin]`` section (see ``monorm_core::settings``). The ``app`` setting names the
``Admin`` object (``module:attr``). The ``setup`` setting names a function (``module:function``, sync or async)
that connects the database, the extensions and the file storage as the application does. Without ``setup``
the admin connects to ``MONORM_DATABASE_URL`` after it imports ``app``.
"""

from __future__ import annotations

import asyncio
import importlib
import inspect
import json
import os
import signal
import sys
from typing import Any

from . import _native


def _load(spec: str, what: str) -> Any:
    module_name, _, attr = spec.partition(":")
    try:
        obj: Any = importlib.import_module(module_name)
        for part in attr.split("."):
            obj = getattr(obj, part)
    except (ImportError, AttributeError) as e:
        raise RuntimeError(f"{what} `{spec}`: {e}") from None
    return obj


async def _serve(config: dict[str, Any]) -> int:
    from .admin import Admin
    from .db import connect, get_database
    from .errors import NotConnected

    admin = _load(config["app"], "admin.app")
    if not isinstance(admin, Admin):
        raise RuntimeError(f"admin.app `{config['app']}` is not a monorm.admin.Admin")
    if config["setup"] is not None:
        result = _load(config["setup"], "admin.setup")()
        if inspect.isawaitable(result):
            await result
    try:
        db = get_database()
    except NotConnected:
        url = os.environ.get("MONORM_DATABASE_URL")
        if not url:
            raise RuntimeError(
                "no database: the setup hook did not connect, and MONORM_DATABASE_URL is not set"
            ) from None
        db = await connect(url)
    try:
        site = admin._site(db._registry)
        server = await site.serve(db._engine, json.dumps(config))
        loop = asyncio.get_running_loop()
        stopping = asyncio.Event()
        for name in (signal.SIGTERM, signal.SIGINT):
            loop.add_signal_handler(name, stopping.set)
        waiting = asyncio.ensure_future(server.wait())
        await asyncio.wait([waiting, asyncio.ensure_future(stopping.wait())], return_when=asyncio.FIRST_COMPLETED)
        if stopping.is_set():
            await server.stop()
        await waiting
    finally:
        await db.close()
    return 0


def run() -> int:
    """Gives the exit code: 0 after a clean stop, 2 for wrong settings, 1 for any other failure."""
    try:
        config = json.loads(_native.admin_settings(os.getcwd()))
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    if os.getcwd() not in sys.path:
        sys.path.insert(0, os.getcwd())
    try:
        return asyncio.run(_serve(config))
    except Exception as e:  # the command reports errors, not tracebacks
        print(f"error: {e}", file=sys.stderr)
        return 1
