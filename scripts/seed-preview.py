#!/usr/bin/env python3
"""Add original demo content to every local preview library.

Only operates on the local development preview, through the regular upload API.
All assets are generated here; no external media is downloaded.
Requires ffmpeg, or PREVIEW_FFMPEG pointing to its executable. Set
PREVIEW_BASE_URL to the server's configured origin when it differs from localhost.
Repeated runs reuse existing named samples instead of duplicating them.
"""
import http.cookiejar
import io
import json
import math
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import urllib.parse
import urllib.request
import wave
import zipfile
import zlib


BASE = os.environ.get("PREVIEW_BASE_URL", f"http://localhost:{os.environ.get('PREVIEW_PORT', '8081')}").rstrip("/")
address = urllib.parse.urlsplit(BASE)
if address.scheme != "http" or address.hostname not in ("localhost", "127.0.0.1", "::1") or address.username or address.password or address.path or address.query or address.fragment:
    raise SystemExit("This script is for a local preview only")
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))


def request(path, method="GET", body=None, content_type="application/json"):
    if body is not None and not isinstance(body, bytes):
        body = json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=body, method=method,
                                 headers={"Origin": BASE, "Content-Type": content_type})
    with opener.open(req) as response:
        data = response.read()
    return json.loads(data) if data else None


def png(width, height, palette, cover=False):
    rows = []
    sky, far, near, sun = palette
    for y in range(height):
        row = bytearray([0])
        for x in range(width):
            t = y / height
            color = tuple(int(v * (1 - t * .12)) for v in sky)
            if (x-width*.74)**2+(y-height*.25)**2 < (width*.065)**2:
                color = sun
            if y > height*(.57+.12*math.sin(x/width*7)):
                color = far
            if y > height*(.76+.09*math.cos(x/width*9+.8)):
                color = near
            if cover and (x < width*.06 or height*.46 < y < height*.47):
                color = tuple(int(v*.7) for v in near)
            row.extend(color)
        rows.append(row)
    def chunk(kind, data):
        return struct.pack(">I",len(data))+kind+data+struct.pack(">I",zlib.crc32(kind+data)&0xffffffff)
    return b"\x89PNG\r\n\x1a\n"+chunk(b"IHDR",struct.pack(">IIBBBBB",width,height,8,2,0,0,0))+chunk(b"IDAT",zlib.compress(b"".join(rows)))+chunk(b"IEND",b"")


def epub(title, palette):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        archive.writestr("META-INF/container.xml", '<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>')
        manifest = '<item id="cover" href="cover.png" media-type="image/png"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>'
        spine = ""
        toc = ""
        for number, heading in enumerate(["从这里开始", "慢下来的片刻", "留住喜欢的事", "下一页的故事"],1):
            manifest += f'<item id="c{number}" href="c{number}.xhtml" media-type="application/xhtml+xml"/>'
            spine += f'<itemref idref="c{number}"/>'
            toc += f'<li><a href="c{number}.xhtml">第{number}章 · {heading}</a></li>'
            paragraphs = ["这是一份为 Revaro 本地预览创作的演示内容。你可以翻页，调整字号，打开目录，也可以一边阅读一边听音乐。",
                          "清晨的光照进窗边，桌上放着一本读到一半的书。我们把忙碌暂时放下，在下一段文字里寻找自己的节奏。",
                          "收藏不必总是很多。有时是一段话，有时是一首旋律，有时是一张让人想起远方的画面。它们都可以安静地留在自己的空间里。",
                          "现在关闭阅读器，再次打开这本书，便可以继续上次的位置。演示书籍和示例图片可以删除，随后导入你自己的收藏。"]
            content=f'<html><body><h1>第{number}章 · {heading}</h1>'+''.join(f'<p>{p}</p>' for p in paragraphs*10)+"</body></html>"
            archive.writestr(f"OEBPS/c{number}.xhtml",content)
        archive.writestr("OEBPS/content.opf",f'<package><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title><dc:creator>Revaro 演示</dc:creator><meta name="cover" content="cover"/></metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>')
        archive.writestr("OEBPS/nav.xhtml",f'<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>{toc}</ol></nav></body></html>')
        archive.writestr("OEBPS/cover.png",png(280,420,palette,True))
    return output.getvalue()


def ambient(root):
    rate = 16000
    data = bytearray()
    for sample in range(rate*45):
        t=sample/rate
        envelope=min(1,t/2,(45-t)/3)*.12
        value=sum(math.sin(2*math.pi*root*r*t)*(.6 if i==0 else .2) for i,r in enumerate([1,1.5,2]))
        data.extend(struct.pack("<h",int(value*envelope*22000)))
    output=io.BytesIO()
    with wave.open(output,"wb") as audio:
        audio.setnchannels(1);audio.setsampwidth(2);audio.setframerate(rate);audio.writeframes(data)
    return output.getvalue()


ROOT = "00000000-0000-0000-0000-000000000000"


def existing(name, parent=ROOT):
    query = urllib.parse.urlencode({"parent_id": parent, "q": name})
    return next((file for file in request(f"/api/files?{query}")["items"] if file["name"] == name), None)


def upload(name, mime, data, parent=ROOT):
    if file := existing(name, parent):
        return file
    session=request("/api/uploads","POST",{"parent_id":parent,"name":name,"size":len(data),"mime_type":mime})
    try:
        request(session['url'],"PUT",data,mime)
        return request(f"/api/uploads/{session['upload_id']}/complete","POST",{})
    except Exception:
        request(f"/api/uploads/{session['upload_id']}","DELETE")
        raise


def video(palette, duration, ffmpeg):
    with tempfile.TemporaryDirectory(prefix="revaro-demo-video-") as directory:
        image, output = Path(directory)/"scene.png", Path(directory)/"scene.webm"
        image.write_bytes(png(960, 540, palette))
        subprocess.run([ffmpeg, "-hide_banner", "-loglevel", "error", "-loop", "1", "-i", str(image),
                        "-t", str(duration), "-vf", "zoompan=z='min(zoom+0.0004,1.12)':d=1:x='iw/2-iw/zoom/2':y='ih/2-ih/zoom/2':s=640x360:fps=24",
                        "-c:v", "libvpx-vp9", "-threads", "2", "-crf", "38", "-b:v", "0", "-an", str(output)], check=True)
        return output.read_bytes()


def main():
    ffmpeg = os.environ.get("PREVIEW_FFMPEG") or shutil.which("ffmpeg")
    if not ffmpeg:
        raise SystemExit("Set PREVIEW_FFMPEG to an ffmpeg executable to generate video samples")
    request("/api/auth/login","POST",{"username":os.environ.get("PREVIEW_USERNAME", "admin"),"password":os.environ.get("PREVIEW_PASSWORD","revaro-preview-2026"),"second_factor":""})
    palettes=[((190,210,191),(116,151,134),(56,102,82),(237,225,164)),((221,198,164),(180,149,116),(113,110,83),(249,229,178)),((178,194,211),(123,147,164),(59,93,117),(246,223,178)),((224,193,188),(164,135,139),(103,104,113),(249,227,185))]
    books=[upload(title+".epub","application/epub+zip",epub(title,palettes[i])) for i,title in enumerate(["窗边慢读","山间随笔","把生活留在这一页"])]
    songs=[upload(title+".wav","audio/wav",ambient(root)) for title,root in [("晨间轻音 · 演示",174),("林间回声 · 演示",196),("夜色渐深 · 演示",146.83)]]
    photos=[upload(title+".png","image/png",png(840,630,palettes[i%4])) for i,title in enumerate(["远山与晨光","落日留白","海边的风","暮色之间","安静的山谷","春日漫步","日落之后","夏天的记忆"])]
    videos=[]
    for i,title in enumerate(["远山慢镜 · 演示","落日微光 · 演示","海风留影 · 演示"]):
        name = title+".webm"
        videos.append(existing(name) or upload(name,"video/webm",video(palettes[i],12+i*6,ffmpeg)))
    print("Books, music, images and videos are ready.", flush=True)
    folder = existing("示例资料") or request("/api/directories", "POST", {"parent_id": ROOT, "name": "示例资料"})
    files=[]
    for name,mime,content in [
        ("预览说明.md", "text/markdown", "# Revaro 预览样例\n\n书籍、音乐、图片、视频和文件均已加入演示内容。\n\n点击顶栏选择按钮进入选择模式，再点击卡片并通过批量操作栏管理。\n\n这些内容由本地脚本生成，可以保留或删除。\n"),
        ("周末出行清单.csv", "text/csv", "项目,数量,备注\n书籍,2,随身阅读\n相机,1,记录风景\n耳机,1,听轻音乐\n"),
        ("收藏配置.json", "application/json", json.dumps({"name":"生活的片刻","theme":"blue-grey","favorites":["阅读","音乐","旅行"]},ensure_ascii=False,indent=2)),
    ]:
        files.append(upload(name,mime,content.encode()))
    upload("旅行计划.md", "text/markdown", "# 周末旅行\n\n- 上午：山间散步\n- 下午：窗边阅读\n- 傍晚：拍摄日落\n".encode(), folder["id"])
    archive=io.BytesIO()
    with zipfile.ZipFile(archive,"w",zipfile.ZIP_DEFLATED) as bundle:
        bundle.writestr("README.md", "Revaro 本地生成的压缩包示例。\n")
    files.append(upload("示例资料包.zip","application/zip",archive.getvalue()))
    collections=request("/api/library/collections")
    for kind,name,items in [("book","想慢慢读的书",books),("audio","阅读时的轻音乐",songs),("image","生活的片刻",photos[:4]),("video","想再看一遍的风景",videos)]:
        collection=next((c for c in collections if c["kind"]==kind and c["name"]==name),None) or request("/api/library/collections","POST",{"kind":kind,"name":name})
        for file in items:
            request(f"/api/library/collections/{collection['id']}/items/{file['id']}","PUT")
    for file in [books[0],songs[0],photos[0],videos[0]]:
        request(f"/api/library/items/{file['id']}","PATCH",{"favorite":True})
    # Keep several examples visible in the home page's recent-content sections.
    for file in books+songs+videos:
        request(f"/api/library/items/{file['id']}","PATCH",{"opened":True})
    for file in files[:2]:
        if not request(f"/api/files/{file['id']}/share")["active"]:
            request(f"/api/files/{file['id']}/share","POST",{"expires_in_seconds":7*86400})
    trashed={file["name"] for file in request("/api/trash")["items"]}
    for name,mime,data in [("旧版预览说明.md","text/markdown","# 旧版说明\n\n这是可恢复的回收站示例。\n".encode()),("未选用的风景.png","image/png",png(420,315,palettes[3]))]:
        if name not in trashed:
            file=upload(name,mime,data)
            request(f"/api/files/{file['id']}","DELETE")
    print("Ready: 3 books, 3 tracks, 8 images, 3 videos, 5 documents/archives, 1 folder, 4 collections, 2 public links and 2 trash examples.")


if __name__ == "__main__":
    main()
