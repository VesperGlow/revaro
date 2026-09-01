// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
package reader

import (
	"container/list"
	"sync"
)

type Cache struct {
	mu       sync.Mutex
	entries  map[string]*list.Element
	order    *list.List
	maxBooks int
	maxBytes int64
}

var DefaultCache = NewCache(3, 96<<20)

func NewCache(maxBooks int, maxBytes int64) *Cache {
	return &Cache{entries: map[string]*list.Element{}, order: list.New(), maxBooks: maxBooks, maxBytes: maxBytes}
}

func (c *Cache) Get(key string) *Book {
	c.mu.Lock()
	defer c.mu.Unlock()
	if el, ok := c.entries[key]; ok {
		c.order.MoveToBack(el)
		return el.Value.(*Book)
	}
	return nil
}

func (c *Cache) Put(key string, b *Book) {
	if b == nil {
		return
	}
	if b.bytes() > c.maxBytes {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if el, ok := c.entries[key]; ok {
		el.Value = b
		c.order.MoveToBack(el)
	} else {
		c.entries[key] = c.order.PushBack(b)
	}
	var total int64
	for el := c.order.Back(); el != nil; el = el.Prev() {
		total += el.Value.(*Book).bytes()
	}
	for c.order.Len() > c.maxBooks || total > c.maxBytes {
		el := c.order.Front()
		book := el.Value.(*Book)
		total -= book.bytes()
		c.order.Remove(el)
		for k, e := range c.entries {
			if e == el {
				delete(c.entries, k)
				break
			}
		}
	}
}
