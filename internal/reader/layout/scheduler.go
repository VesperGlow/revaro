package layout

import (
	"context"
	"encoding/base64"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/chromedp/cdproto/fetch"
	"github.com/chromedp/cdproto/network"
	"github.com/chromedp/chromedp"
)

// Scheduler 是 headless Chromium 的受控执行器：
//   - 单实例复用：整个进程只启动一个浏览器，任务在独立 tab 里串行执行，
//     避免每次分页都付一次浏览器冷启动（数百毫秒～秒级）；
//   - 受控队列：一个串行槽位，任务排队执行；调用方用 ctx 取消排队/运行中的
//     任务（取消只关 tab，不杀浏览器）；
//   - 资源统计：任务数、页数、耗时与浏览器进程 RSS（Linux /proc）。
//
// Fetch 拦截监听器只注册一次（浏览器级），通过原子指针读取当前任务的
// resolver，任务间互不干扰。
type Scheduler struct {
	chromePath string

	mu            sync.Mutex
	browserCtx    context.Context
	browserCancel context.CancelFunc
	browserPID    int
	closed        bool

	resolver atomic.Pointer[resolverBox]
	slots    chan struct{}

	runs        atomic.Int64
	pages       atomic.Int64
	lastSeconds atomic.Int64 // 最近一次任务耗时（纳秒）
}

type resolverBox struct {
	fn func(url string) (body []byte, mime string, ok bool)
}

// SchedulerStats 暴露给 system/status 的调度器统计。
type SchedulerStats struct {
	Available      bool    `json:"available"`
	BrowserPID     int     `json:"browser_pid,omitempty"`
	BrowserRSS     int64   `json:"browser_rss_bytes,omitempty"`
	QueueLength    int     `json:"queue_length"`
	Runs           int64   `json:"runs"`
	GeneratedPages int64   `json:"generated_pages"`
	LastSeconds    float64 `json:"last_job_seconds"`
}

// NewScheduler 构造调度器；浏览器按需启动（首个任务）。
func NewScheduler(chromePath string) *Scheduler {
	if chromePath == "" {
		chromePath = DetectEngine().Path
	}
	return &Scheduler{chromePath: chromePath, slots: make(chan struct{}, 1)}
}

// Available 报告浏览器可用性（路径已探测到）。
func (s *Scheduler) Available() bool { return s.chromePath != "" }

// Stats 返回当前统计快照。
func (s *Scheduler) Stats() SchedulerStats {
	out := SchedulerStats{
		Available:      s.chromePath != "",
		QueueLength:    len(s.slots),
		Runs:           s.runs.Load(),
		GeneratedPages: s.pages.Load(),
		LastSeconds:    float64(s.lastSeconds.Load()) / float64(time.Second),
	}
	s.mu.Lock()
	out.BrowserPID = s.browserPID
	pid := s.browserPID
	s.mu.Unlock()
	if pid > 0 {
		out.BrowserRSS = processRSS(pid)
	}
	return out
}

// Close 终止浏览器（进程退出前调用）。
func (s *Scheduler) Close() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return
	}
	s.closed = true
	if s.browserCancel != nil {
		s.browserCancel()
		s.browserCancel = nil
	}
	s.browserCtx = nil
}

// Run 在共享浏览器的一个新 tab 里串行执行任务：
//   - resolve 为任务提供请求响应体（wrapper 文档/字体/图片），仅本任务有效；
//   - fn 拿到该 tab 的会话 context，用 chromedp 在其上求值；
//   - ctx 取消会中止排队或运行中的任务（关 tab，浏览器保留）。
func (s *Scheduler) Run(ctx context.Context, resolve func(url string) ([]byte, string, bool), fn func(session context.Context) error) error {
	if !s.Available() {
		return fmt.Errorf("layout engine unavailable: %s", DetectEngine().Reason)
	}
	select {
	case s.slots <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	defer func() { <-s.slots }()

	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return fmt.Errorf("layout scheduler is closed")
	}
	if s.browserCtx == nil {
		launchCtx, err := s.launch()
		if err != nil {
			s.mu.Unlock()
			return err
		}
		s.browserCtx = launchCtx
	}
	browserCtx := s.browserCtx
	s.mu.Unlock()

	// 每个任务一个 tab：tab 从共享浏览器派生（继承 allocator/browser），
	// 通过 AfterFunc 与任务 ctx 联动——取消任务只关这个 tab，不牵连浏览器。
	session, sessionCancel := chromedp.NewContext(browserCtx)
	defer sessionCancel()
	stopCancel := context.AfterFunc(ctx, sessionCancel)
	defer stopCancel()

	// Fetch 拦截监听挂在 tab 级：Fetch 事件只发给发起请求的 tab 会话。
	// 任务串行执行，任意时刻只有一个 tab 在发请求；resolver 原子指针
	// 保证监听器读到的总是当前任务。
	chromedp.ListenTarget(session, func(ev any) {
		e, ok := ev.(*fetch.EventRequestPaused)
		if !ok {
			return
		}
		go func() {
			box := s.resolver.Load()
			if box == nil || box.fn == nil {
				_ = chromedp.Run(session, fetch.FailRequest(e.RequestID, network.ErrorReasonBlockedByClient))
				return
			}
			body, mime, ok := box.fn(e.Request.URL)
			if !ok {
				_ = chromedp.Run(session, fetch.FailRequest(e.RequestID, network.ErrorReasonBlockedByClient))
				return
			}
			_ = chromedp.Run(session, fetch.FulfillRequest(e.RequestID, 200).
				WithResponseHeaders([]*fetch.HeaderEntry{{Name: "Content-Type", Value: mime}}).
				WithBody(base64.StdEncoding.EncodeToString(body)))
		}()
	})

	s.resolver.Store(&resolverBox{fn: resolve})
	defer s.resolver.Store(nil)

	started := time.Now()
	s.runs.Add(1)
	err := fn(session)
	s.lastSeconds.Store(time.Since(started).Nanoseconds())
	return err
}

// launch 启动浏览器并安装唯一一个 Fetch 拦截监听器。
func (s *Scheduler) launch() (context.Context, error) {
	opts := append(chromedp.DefaultExecAllocatorOptions[:],
		chromedp.ExecPath(s.chromePath),
		chromedp.NoSandbox, // 只渲染白名单清洗内容，且容器内非 root 无用户命名空间
		chromedp.Flag("hide-scrollbars", true),
		chromedp.Flag("font-render-hinting", "none"),
	)
	allocCtx, _ := chromedp.NewExecAllocator(context.Background(), opts...)
	browserCtx, cancel := chromedp.NewContext(allocCtx, chromedp.WithErrorf(func(string, ...any) {}))
	s.browserCancel = cancel

	// 首次连接以拿到浏览器进程 PID
	if err := chromedp.Run(browserCtx, chromedp.Navigate("about:blank")); err != nil {
		cancel()
		return nil, fmt.Errorf("launch chromium: %w", err)
	}
	if b := chromedp.FromContext(browserCtx); b != nil && b.Browser != nil && b.Browser.Process() != nil {
		s.browserPID = b.Browser.Process().Pid
	}

	return browserCtx, nil
}

// AddPages 累计已生成页数（服务端每写一页调用）。
func (s *Scheduler) AddPages(n int64) { s.pages.Add(n) }

// processRSS 读 /proc/{pid}/status 的 VmRSS（Linux）；其他平台返回 0。
func processRSS(pid int) int64 {
	data, err := os.ReadFile(fmt.Sprintf("/proc/%d/status", pid))
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(data), "\n") {
		if !strings.HasPrefix(line, "VmRSS:") {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) >= 2 {
			kb, err := strconv.ParseInt(fields[1], 10, 64)
			if err == nil {
				return kb * 1024
			}
		}
	}
	return 0
}
