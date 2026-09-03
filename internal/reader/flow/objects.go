package flow

import (
	"crypto/sha256"
	"encoding/hex"
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

// VersionDir 返回当前 flow 格式版本目录名（f<N>）。
func VersionDir() string { return fmt.Sprintf("f%d", FlowFormatVersion) }

// ManifestObjectKey 返回 flow manifest 的对象键。
func ManifestObjectKey(bookObjectKey string) string {
	return fmt.Sprintf("%s%s/manifest.json", ObjectPrefix(bookObjectKey), VersionDir())
}

// ChunkObjectKey 返回第 index 个 flow chunk 的对象键。
func ChunkObjectKey(bookObjectKey string, index int) string {
	return fmt.Sprintf("%s%schunks/%d.html", ObjectPrefix(bookObjectKey), VersionDir(), index)
}

// BookKeyFromObject 从 flow 对象键反解书的 blob 键（"blobs/<uuid>"）：
// 形如 flows/blobs/<uuid>/fN/...，前两段即 blob 键；非 flow 键返回空串。
func BookKeyFromObject(key string) string { return bookKeyFromObject(key) }

// BookFingerprint 返回书 blob 键的短内容指纹（sha256 前 16 字节的 hex）：
// 服务端写入 manifest.book_key，客户端持久缓存用它隔离 chunk 键——文件
// 内容变化（同 id 重传）时指纹改变，旧 chunk 缓存自然失效。
func BookFingerprint(bookObjectKey string) string {
	sum := sha256.Sum256([]byte(bookObjectKey))
	return hex.EncodeToString(sum[:8])
}

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
