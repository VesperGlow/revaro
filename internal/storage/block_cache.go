package storage

import (
	"container/list"
	"sync"
)

type blockCacheEntry struct {
	id   string
	data []byte
}

// blockLRU keeps immutable content-addressed blocks shared by all logical file
// readers. In particular, an ffprobe request followed by an FFmpeg request can
// reuse the same S3 GETs without allowing cache memory to grow without bound.
type blockLRU struct {
	mu       sync.Mutex
	capacity int64
	size     int64
	items    map[string]*list.Element
	order    *list.List
}

func newBlockLRU(capacity int64) *blockLRU {
	return &blockLRU{capacity: capacity, items: make(map[string]*list.Element), order: list.New()}
}

func (c *blockLRU) get(id string) ([]byte, bool) {
	if c == nil || c.capacity <= 0 {
		return nil, false
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	element := c.items[id]
	if element == nil {
		return nil, false
	}
	c.order.MoveToFront(element)
	return element.Value.(*blockCacheEntry).data, true
}

func (c *blockLRU) put(id string, data []byte) {
	if c == nil || len(data) == 0 || int64(len(data)) > c.capacity {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if element := c.items[id]; element != nil {
		entry := element.Value.(*blockCacheEntry)
		c.size += int64(len(data) - len(entry.data))
		entry.data = data
		c.order.MoveToFront(element)
	} else {
		c.items[id] = c.order.PushFront(&blockCacheEntry{id: id, data: data})
		c.size += int64(len(data))
	}
	for c.size > c.capacity {
		element := c.order.Back()
		if element == nil {
			break
		}
		entry := element.Value.(*blockCacheEntry)
		delete(c.items, entry.id)
		c.order.Remove(element)
		c.size -= int64(len(entry.data))
	}
}
