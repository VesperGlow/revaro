package flow

import (
	"fmt"
	"strings"
)

// flow 产物在对象存储中的布局（内容级不可变缓存）：
//
//	flows/<bookObjectKey>/f{version}/manifest.json
//	flows/<bookObjectKey>/f{version}/chunks/{index}.html
//
// bookObjectKey 形如 "blobs/<uuid>"，由文件内容哈希唯一决定，因此整棵
// flows 子树的内容都随书内容固定；flow 生成语义变化时递增 FlowFormatVersion，
// 新版本落在新的 f{version} 目录里，旧产物由 GC 按 TTL/容量回收。

const objectRoot = "flows/"

// ObjectPrefix 返回一本书全部 flow 产物的对象键前缀。
func ObjectPrefix(bookObjectKey string) string {
	return objectRoot + bookObjectKey + "/"
}

func versionDir() string { return fmt.Sprintf("f%d", FlowFormatVersion) }

// ManifestObjectKey 返回 flow manifest 的对象键。
func ManifestObjectKey(bookObjectKey string) string {
	return fmt.Sprintf("%s%s/manifest.json", ObjectPrefix(bookObjectKey), versionDir())
}

// ChunkObjectKey 返回第 index 个 flow chunk 的对象键。
func ChunkObjectKey(bookObjectKey string, index int) string {
	return fmt.Sprintf("%s%schunks/%d.html", ObjectPrefix(bookObjectKey), versionDir(), index)
}

// BookKeyFromObject 从 flow 对象键反解书的 blob 键（"blobs/<uuid>"）：
// 形如 flows/blobs/<uuid>/fN/...，前两段即 blob 键；非 flow 键返回空串。
func BookKeyFromObject(key string) string { return bookKeyFromObject(key) }

func bookKeyFromObject(key string) string {
	rest := strings.TrimPrefix(key, objectRoot)
	if rest == key {
		return ""
	}
	head, tail, ok := strings.Cut(rest, "/")
	if !ok || head != "blobs" {
		return ""
	}
	uuidPart, _, ok := strings.Cut(tail, "/")
	if !ok {
		return ""
	}
	return "blobs/" + uuidPart
}
