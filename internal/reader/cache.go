// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
//
// 解析 Book 的内存 LRU 由 Server 持有并注册进统一 Global CacheManager
//（class reader/books）：保留自身「对象缓存」策略，但容量与统计纳入
// 全局协调（见 internal/cache）。
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
	bytes    int64
}

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
		old := el.Value.(*Book)
		c.bytes -= old.bytes()
		el.Value = b
		c.order.MoveToBack(el)
		c.bytes += b.bytes()
	} else {
		c.entries[key] = c.order.PushBack(b)
		c.bytes += b.bytes()
	}
	for c.order.Len() > c.maxBooks || c.bytes > c.maxBytes {
		el := c.order.Front()
		if el == nil {
			break
		}
		book := el.Value.(*Book)
		c.bytes -= book.bytes()
		c.order.Remove(el)
		for k, e := range c.entries {
			if e == el {
				delete(c.entries, k)
				break
			}
		}
	}
}

// Stats 返回当前占用（字节、条目数），供统一缓存管理器汇总。
func (c *Cache) Stats() (int64, int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.bytes, c.order.Len()
}

// Trim 收敛容量（Put 时已逐次淘汰；这里防御性再跑一遍，供全局 pruner
// 周期触发）。
func (c *Cache) Trim() {
	c.mu.Lock()
	defer c.mu.Unlock()
	for c.order.Len() > c.maxBooks || c.bytes > c.maxBytes {
		el := c.order.Front()
		if el == nil {
			break
		}
		book := el.Value.(*Book)
		c.bytes -= book.bytes()
		c.order.Remove(el)
		for k, e := range c.entries {
			if e == el {
				delete(c.entries, k)
				break
			}
		}
	}
}
