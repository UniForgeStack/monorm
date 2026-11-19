"""The text form of a Duration: ``[-][D ]HH:MM:SS[.ffffff]``. The Rust core (`monorm_core::duration`) has the same rules."""

from __future__ import annotations

import datetime
import re

_TEXT = re.compile(r"([-+])?(?:(\d+) )?(\d+):([0-5]\d):([0-5]\d)(?:\.(\d{1,6}))?", re.ASCII)
_ISO = re.compile(r"([-+])?PT(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)(?:\.(\d{1,6}))?S)?", re.ASCII | re.IGNORECASE)
_I64 = 2**63


def format_duration(value: datetime.timedelta) -> str:
    micros = (value.days * 86_400 + value.seconds) * 1_000_000 + value.microseconds
    sign = "-" if micros < 0 else ""
    rest = abs(micros)
    days, rest = divmod(rest, 86_400_000_000)
    hours, rest = divmod(rest, 3_600_000_000)
    minutes, rest = divmod(rest, 60_000_000)
    seconds, frac = divmod(rest, 1_000_000)
    out = f"{sign}{days} " if days else sign
    out += f"{hours:02}:{minutes:02}:{seconds:02}"
    return out + (f".{frac:06}" if frac else "")


def parse_duration(text: str) -> datetime.timedelta:
    """Parse ``[-][D ]H+:MM:SS[.f]`` or ``[-]PT[nH][nM][n[.f]S]``; a ``ValueError`` for other text."""
    text = text.strip()
    if m := _TEXT.fullmatch(text):
        sign, days, hours, minutes, seconds, frac = m.groups()
        parts = (int(days or 0), int(hours), int(minutes), int(seconds))
    elif (m := _ISO.fullmatch(text)) and any(m.groups()[1:]):
        sign, hours, minutes, seconds, frac = m.groups()
        parts = (0, int(hours or 0), int(minutes or 0), int(seconds or 0))
    else:
        raise ValueError(f"not a duration: {text!r}")
    days, hours, minutes, seconds = parts
    micros = (((days * 24 + hours) * 60 + minutes) * 60 + seconds) * 1_000_000 + int((frac or "").ljust(6, "0") or 0)
    micros = -micros if sign == "-" else micros
    if not -_I64 <= micros < _I64:
        raise ValueError(f"duration out of range: {text!r}")
    return datetime.timedelta(microseconds=micros)
