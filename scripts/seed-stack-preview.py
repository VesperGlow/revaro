#!/usr/bin/env python3
"""Add 24 original books and five explicit Stacks to a local preview.

Uses normal upload, shelf and Stack APIs. Repeated runs reuse named samples.
PREVIEW_BASE_URL / PREVIEW_USERNAME / PREVIEW_PASSWORD match seed-preview.py.
No ffmpeg or third-party Python packages are needed.
"""
import html
import importlib.util
import io
import json
from pathlib import Path
import sys
import zipfile

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("seed_preview", Path(__file__).with_name("seed-preview.py"))
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)

PALETTES = [
    ((98, 163, 140), (57, 116, 96), (29, 73, 66), (248, 224, 151)),
    ((233, 174, 100), (203, 125, 73), (124, 72, 54), (255, 236, 172)),
    ((68, 111, 159), (44, 77, 122), (28, 45, 78), (255, 220, 143)),
    ((226, 138, 122), (182, 91, 88), (105, 56, 68), (255, 224, 173)),
    ((174, 149, 206), (120, 96, 165), (63, 61, 103), (247, 226, 179)),
    ((83, 160, 178), (45, 119, 144), (25, 71, 95), (251, 231, 180)),
    ((237, 218, 164), (174, 159, 94), (100, 111, 70), (255, 244, 202)),
    ((206, 162, 184), (155, 111, 149), (85, 67, 107), (253, 226, 179)),
]

GROUPS = [
    ("山野双册 · 示例", ["山里的清晨", "森林来信"]),
    ("城市漫游 · 示例", ["街角咖啡馆", "旧城与新雨", "沿河散步"]),
    ("设计与日常 · 示例", ["留白的练习", "日常的形状", "色彩札记", "纸上花园"]),
    ("旅行书单 · 示例", ["海边的慢时光", "远方的车站", "岛屿手记", "沙漠里的星星", "秋日公路"]),
    ("四季自然笔记 · 示例", ["春天的种子", "夏夜萤火", "落叶观察", "冬日微光", "河流的声音", "山谷里的风"]),
]
SINGLES = ["一杯茶的时间", "星海探险·启航", "星海探险·远航", "星海探险·归航"]


def book_bytes(title, palette_index, series=None, volume=None):
    """Keep the existing readable demo chapters and add optional recommendation metadata."""
    original = preview.epub(title, PALETTES[palette_index % len(PALETTES)])
    output = io.BytesIO()
    with zipfile.ZipFile(io.BytesIO(original)) as source, zipfile.ZipFile(output, "w") as target:
        for entry in source.infolist():
            content = source.read(entry.filename)
            if series and entry.filename == "OEBPS/content.opf":
                metadata = (
                    f'<meta property="belongs-to-collection" id="demo-series">{html.escape(series)}</meta>'
                    '<meta refines="#demo-series" property="collection-type">series</meta>'
                    f'<meta refines="#demo-series" property="group-position">{volume}</meta>'
                )
                content = content.decode().replace("</metadata>", metadata + "</metadata>").encode()
            target.writestr(entry, content)
    return output.getvalue()


def main():
    import os

    preview.request("/api/auth/login", "POST", {
        "username": os.environ.get("PREVIEW_USERNAME", "admin"),
        "password": os.environ.get("PREVIEW_PASSWORD", "revaro-preview-2026"),
        "second_factor": "",
    })
    collections = preview.request("/api/library/collections")
    shelf = next((c for c in collections if c["kind"] == "book" and c["name"] == "想慢慢读的书"), None)
    if shelf is None:
        shelf = next((c for c in collections if c["kind"] == "book" and c["name"] == "Stack 示例书架"), None)
    if shelf is None:
        shelf = preview.request("/api/library/collections", "POST", {"kind": "book", "name": "Stack 示例书架"})
    existing_stacks = preview.request("/api/library/stacks")
    files = []
    stacks = []
    number = 0
    for name, titles in GROUPS:
        members = []
        for title in titles:
            filename = f"{title} · 示例.epub"
            file = preview.existing(filename) or preview.upload(filename, "application/epub+zip", book_bytes(title, number))
            number += 1
            preview.request(f"/api/library/collections/{shelf['id']}/items/{file['id']}", "PUT")
            files.append(file)
            members.append(file)
        ids = [file["id"] for file in members]
        # Explicit demo action, independent of series metadata. Keep manual edits on reruns.
        stack = next((s for s in existing_stacks if s["name"] == name), None)
        if stack is None:
            grouped_ids = {file["id"] for s in existing_stacks for file in s["files"]}
            if any(file_id in grouped_ids for file_id in ids):
                print(f"Preserving an existing manual grouping for {name}", flush=True)
                continue
            stack = preview.request("/api/library/stacks", "POST", {"name": name, "file_ids": ids})
            existing_stacks.append(stack)
        stacks.append(stack)
        print(f"{name}: {len(stack['files'])} books", flush=True)
    for index, title in enumerate(SINGLES):
        filename = f"{title} · 示例.epub"
        series = "星海探险 · 示例系列" if index else None
        file = preview.existing(filename) or preview.upload(filename, "application/epub+zip", book_bytes(title, number, series, index))
        number += 1
        preview.request(f"/api/library/collections/{shelf['id']}/items/{file['id']}", "PUT")
        files.append(file)
    # Generate covers through the regular authenticated thumbnail endpoint.
    for file in files:
        with preview.opener.open(preview.BASE + f"/api/files/{file['id']}/thumbnail") as response:
            if response.status != 200 or not response.headers.get_content_type().startswith("image/"):
                raise RuntimeError(f"Cover unavailable: {file['name']}")
            response.read()
    summary = {
        "books": len(files), "stacks": [{"name": s["name"], "count": len(s["files"])} for s in stacks],
        "standalone_samples": len(SINGLES), "shelf": shelf["name"], "url": preview.BASE + "/library",
    }
    print(json.dumps(summary, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
