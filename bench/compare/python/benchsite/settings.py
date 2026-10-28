"""A minimal Django project for the benchmark: one app, no admin, auth, sessions or middleware."""

import os
from urllib.parse import urlparse

from common import POOL_SIZE

_url = urlparse(os.environ["BENCH_DATABASE_URL"])

SECRET_KEY = "benchmark-only"
DEBUG = False  # DEBUG=True records every query and skews timings
USE_TZ = True
INSTALLED_APPS = ["blog"]
DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"
DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.postgresql",
        "NAME": _url.path.lstrip("/"),
        "USER": _url.username,
        "PASSWORD": _url.password,
        "HOST": _url.hostname,
        "PORT": _url.port,
        # psycopg 3 pool of the same size as monorm.connect(max_connections=...); prepared statements stay at
        # psycopg's default (prepare_threshold=5).
        "OPTIONS": {"pool": {"min_size": POOL_SIZE, "max_size": POOL_SIZE}},
    }
}
