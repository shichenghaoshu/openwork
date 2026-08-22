#!/usr/bin/env python3
"""Bounded, shell-free adapter from an OpenWork capability call to CLI-Anything."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

MAX_STDIN = 256 * 1024
MAX_TEXT = 4096
MAX_ROWS = 200
MAX_COLUMNS = 52
MAX_SLIDES = 40
OUTPUT = Path("/workspace/output")
CLI = "/usr/local/bin/cli-anything-libreoffice"
SAFE_FILE = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}")


def fail(message: str) -> None:
    print(json.dumps({"ok": False, "error": message}), file=sys.stderr)
    raise SystemExit(2)


def text(value: object, field: str) -> str:
    if not isinstance(value, str) or len(value.encode("utf-8")) > MAX_TEXT:
        fail(f"invalid {field}")
    return value


def filename(value: object, suffix: str) -> str:
    name = text(value, "filename")
    if not SAFE_FILE.fullmatch(name) or not name.lower().endswith(suffix):
        fail("invalid output filename")
    return name


def run(arguments: list[str]) -> None:
    result = subprocess.run(
        [CLI, "--json", *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=45,
        check=False,
        env={"HOME": "/tmp", "PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8"},
    )
    if result.returncode != 0:
        fail("LibreOffice capability command failed")


def column_name(index: int) -> str:
    name = ""
    current = index + 1
    while current:
        current, remainder = divmod(current - 1, 26)
        name = chr(65 + remainder) + name
    return name


def create_spreadsheet(spec: object, title: str) -> str:
    if not isinstance(spec, dict) or set(spec) - {"filename", "sheet", "rows"}:
        fail("invalid spreadsheet request")
    output_name = filename(spec.get("filename", "report.xlsx"), ".xlsx")
    sheet = text(spec.get("sheet", "Report"), "sheet")
    rows = spec.get("rows")
    if not isinstance(rows, list) or not rows or len(rows) > MAX_ROWS:
        fail("invalid spreadsheet rows")
    project = OUTPUT / ".openwork-spreadsheet.json"
    run(["document", "new", "--type", "calc", "--name", title, "--output", str(project)])
    run(["--project", str(project), "calc", "rename-sheet", "0", sheet])
    for row_index, row in enumerate(rows, 1):
        if not isinstance(row, list) or not row or len(row) > MAX_COLUMNS:
            fail("invalid spreadsheet row")
        for column_index, value in enumerate(row):
            cell = f"{column_name(column_index)}{row_index}"
            run(["--project", str(project), "calc", "set-cell", cell, text(value, "cell")])
    destination = OUTPUT / output_name
    run(["--project", str(project), "export", "render", str(destination), "--preset", "xlsx", "--overwrite"])
    project.unlink(missing_ok=True)
    return output_name


def create_presentation(spec: object, title: str) -> str:
    if not isinstance(spec, dict) or set(spec) - {"filename", "slides"}:
        fail("invalid presentation request")
    output_name = filename(spec.get("filename", "briefing.pptx"), ".pptx")
    slides = spec.get("slides")
    if not isinstance(slides, list) or not slides or len(slides) > MAX_SLIDES:
        fail("invalid presentation slides")
    project = OUTPUT / ".openwork-presentation.json"
    run(["document", "new", "--type", "impress", "--name", title, "--output", str(project)])
    for slide in slides:
        if not isinstance(slide, dict) or set(slide) != {"title", "content"}:
            fail("invalid presentation slide")
        run([
            "--project", str(project), "impress", "add-slide",
            "--title", text(slide["title"], "slide title"),
            "--content", text(slide["content"], "slide content"),
        ])
    destination = OUTPUT / output_name
    run(["--project", str(project), "export", "render", str(destination), "--preset", "pptx", "--overwrite"])
    project.unlink(missing_ok=True)
    return output_name


def main() -> None:
    raw = sys.stdin.buffer.read(MAX_STDIN + 1)
    if not raw or len(raw) > MAX_STDIN:
        fail("invalid capability input")
    try:
        request = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        fail("invalid capability input")
    if not isinstance(request, dict) or set(request) != {"title", "spreadsheet", "presentation"}:
        fail("invalid capability input")
    title = text(request["title"], "title")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    artifacts = [
        create_spreadsheet(request["spreadsheet"], title),
        create_presentation(request["presentation"], title),
    ]
    print(json.dumps({"ok": True, "artifacts": artifacts}, separators=(",", ":")))


if __name__ == "__main__":
    main()
