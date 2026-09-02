package layout

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/reader"
)

// TestSchedulerReusesBrowser 验证调度器的两项 Phase 4 目标：
//  1. 实例复用：多次分页共用一个浏览器进程（PID 不变、启动开销只付一次）；
//  2. 受控串行：任务逐个执行，同内容分页结果逐字节一致（跨 tab 确定性）。
func TestSchedulerReusesBrowser(t *testing.T) {
	info := DetectEngine()
	if !info.Available {
		t.Skipf("chromium unavailable: %s", info.Reason)
	}
	sched := NewScheduler(info.Path)
	defer sched.Close()
	pager := NewPager(info.Path, "/api/files/f1/book/assets", []reader.Asset{})

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	profile := Profile{ViewportW: 400, ViewportH: 520, FontSize: 15, FontFamily: FontFamilySerif, LineHeight: 1.6, MarginTop: 16, MarginBottom: 12, MarginSide: 16}
	chapter := ChapterInput{Spine: 0, HTML: "<p>" + strings.Repeat("山川异域风月同天", 120) + "</p>"}

	first, err := pager.PaginateSpine(ctx, sched, chapter, profile, false, nil)
	if err != nil {
		t.Fatalf("first pagination: %v", err)
	}
	if len(first.Pages) == 0 {
		t.Fatal("first pagination produced no pages")
	}
	pidAfterFirst := sched.Stats().BrowserPID

	second, err := pager.PaginateSpine(ctx, sched, chapter, profile, false, nil)
	if err != nil {
		t.Fatalf("second pagination: %v", err)
	}
	stats := sched.Stats()
	if stats.BrowserPID == 0 || stats.BrowserPID != pidAfterFirst {
		t.Fatalf("browser instance not reused: first=%d second=%d", pidAfterFirst, stats.BrowserPID)
	}
	if stats.Runs != 2 {
		t.Fatalf("runs=%d, want 2", stats.Runs)
	}
	if stats.GeneratedPages != int64(len(first.Pages)+len(second.Pages)) {
		t.Fatalf("generated pages=%d", stats.GeneratedPages)
	}
	if len(first.Pages) != len(second.Pages) {
		t.Fatalf("cross-tab determinism: %d != %d pages", len(first.Pages), len(second.Pages))
	}
	for i := range first.Pages {
		if first.Pages[i].Start.Compare(second.Pages[i].Start) != 0 ||
			first.Pages[i].End.Compare(second.Pages[i].End) != 0 ||
			first.Pages[i].HTML != second.Pages[i].HTML {
			t.Fatalf("cross-tab page %d differs", i)
		}
	}
	if stats.BrowserRSS <= 0 {
		t.Logf("browser rss not measurable (non-Linux?): %d", stats.BrowserRSS)
	}
}
