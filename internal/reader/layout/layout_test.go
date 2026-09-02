package layout

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestProfileIDStableAndDistinct(t *testing.T) {
	p := Profile{ViewportW: 390, ViewportH: 844, FontSize: 21, FontFamily: FontFamilySerif, LineHeight: 1.7, MarginTop: 28, MarginBottom: 20, MarginSide: 22}
	id1 := p.ID("hash-a")
	id2 := p.ID("hash-a")
	if id1 != id2 {
		t.Fatalf("profile id must be stable: %q != %q", id1, id2)
	}
	if !strings.HasPrefix(id1, "v1-") {
		t.Fatalf("profile id should carry format version: %q", id1)
	}
	if p.ID("hash-b") == id1 {
		t.Fatal("different book hash must yield different id")
	}
	q := p
	q.FontSize = 22
	if q.ID("hash-a") == id1 {
		t.Fatal("different font size must yield different id")
	}
}

func TestProfileNormalizedDefaults(t *testing.T) {
	n := Profile{}.normalized()
	if n.ViewportW != 800 || n.ViewportH != 600 || n.FontSize != 19 || n.LineHeight != 1.7 {
		t.Fatalf("defaults wrong: %+v", n)
	}
	p := Profile{ViewportW: 320, ViewportH: 480}
	n = p.normalized()
	if n.ViewportW != 320 || n.FontSize != 19 || n.FontFamily != FontFamilySerif {
		t.Fatalf("partial profile normalized wrong: %+v", n)
	}
	if n.InnerWidth() != 320-2*n.MarginSide {
		t.Fatalf("inner width wrong: %d", n.InnerWidth())
	}
}

func TestProfileValidate(t *testing.T) {
	valid := Profile{ViewportW: 390, ViewportH: 844, FontSize: 20, FontFamily: FontFamilySerif, LineHeight: 1.6, MarginTop: 20, MarginBottom: 16, MarginSide: 20}
	if err := valid.Validate(); err != nil {
		t.Fatalf("valid profile rejected: %v", err)
	}
	cases := []struct {
		name   string
		mutate func(*Profile)
	}{
		{"tiny viewport", func(p *Profile) { p.ViewportW = 10 }},
		{"huge viewport", func(p *Profile) { p.ViewportH = 20000 }},
		{"tiny font", func(p *Profile) { p.FontSize = 4 }},
		{"bad family", func(p *Profile) { p.FontFamily = `x");}` }},
		{"line too loose", func(p *Profile) { p.LineHeight = 0.5 }},
		{"margins eat area", func(p *Profile) { p.MarginSide = 200 }},
	}
	for _, tc := range cases {
		p := valid
		tc.mutate(&p)
		if err := p.Validate(); err == nil {
			t.Errorf("%s: expected error, got nil", tc.name)
		}
	}
}

func TestAnchorCompare(t *testing.T) {
	a := func(spine int, path []int, off int) Anchor { return Anchor{Spine: spine, Path: path, Offset: off} }
	order := []Anchor{
		a(0, []int{0}, -1),
		a(0, []int{0}, 0),
		a(0, []int{0}, 5),
		a(0, []int{0, 0}, 0),
		a(0, []int{0, 1}, 0),
		a(0, []int{1}, -1),
		a(1, []int{}, -1),
		a(1, []int{0}, 0),
	}
	for i := 0; i < len(order); i++ {
		if !order[i].Valid() {
			t.Fatalf("anchor %d invalid: %+v", i, order[i])
		}
		if order[i].Compare(order[i]) != 0 {
			t.Fatalf("anchor self compare != 0: %+v", order[i])
		}
		for j := i + 1; j < len(order); j++ {
			if order[i].Compare(order[j]) >= 0 {
				t.Fatalf("anchors out of order: %+v >= %+v", order[i], order[j])
			}
			if order[j].Compare(order[i]) <= 0 {
				t.Fatalf("anchors reverse compare wrong: %+v", order[j])
			}
		}
	}
	if a(0, nil, 0).Compare(a(0, []int{0}, -1)) >= 0 {
		t.Fatal("book start anchor must sort before first child")
	}
}

func TestAnchorValid(t *testing.T) {
	bad := []Anchor{
		{Spine: -1},
		{Spine: 0, Path: []int{-1}},
		{Spine: 0, Path: make([]int, 65)},
		{Spine: 0, Offset: -2},
	}
	for i, a := range bad {
		if a.Valid() {
			t.Errorf("anchor %d should be invalid: %+v", i, a)
		}
	}
}

func TestManifestPageForAnchor(t *testing.T) {
	mk := func(spine int, path ...int) Anchor { return Anchor{Spine: spine, Path: path, Offset: 0} }
	m := &Manifest{
		PageCount: 3,
		Pages: []PageMeta{
			{Index: 0, Start: mk(0, 0), End: mk(0, 1)},
			{Index: 1, Start: mk(0, 1), End: mk(1, 0)},
			{Index: 2, Start: mk(1, 0), End: mk(2, 0)},
		},
	}
	for _, want := range []struct {
		anchor Anchor
		page   int
	}{
		{mk(0, 0), 0},
		{mk(0, 0, 5), 0},
		{mk(0, 1), 1}, // 页边界归属下一页
		{mk(0, 2), 1},
		{mk(1, 0), 2},
		{mk(1, 9, 9), 2},
		{mk(3, 0), 2}, // 书末之后 → 最后一页
	} {
		if got := m.PageForAnchor(want.anchor); got != want.page {
			t.Errorf("PageForAnchor(%+v) = %d, want %d", want.anchor, got, want.page)
		}
	}
	if got := m.PageForAnchor(mk(0)); got != 0 {
		t.Errorf("pre-book anchor should map to first page, got %d", got)
	}
	empty := &Manifest{}
	if got := empty.PageForAnchor(mk(0)); got != -1 {
		t.Errorf("empty manifest should return -1, got %d", got)
	}
	if a, ok := m.AnchorForPage(1); !ok || a.Compare(mk(0, 1)) != 0 {
		t.Errorf("AnchorForPage(1) = %+v ok=%v", a, ok)
	}
}

func TestAnchorJSONRoundTrip(t *testing.T) {
	a := Anchor{Spine: 3, Path: []int{1, 4, 2}, Offset: -1}
	data, err := json.Marshal(a)
	if err != nil {
		t.Fatal(err)
	}
	var back Anchor
	if err := json.Unmarshal(data, &back); err != nil {
		t.Fatal(err)
	}
	if back.Compare(a) != 0 {
		t.Fatalf("round trip mismatch: %+v != %+v", back, a)
	}
	if string(data) != `{"spine":3,"path":[1,4,2],"offset":-1}` {
		t.Fatalf("unexpected JSON: %s", data)
	}
}

func TestTXTChapters(t *testing.T) {
	text := "第一章 开始\n正文\n第二章 继续\n"
	chapters := TXTChapters(text, []int64{0, 7})
	if len(chapters) != 2 {
		t.Fatalf("chapters = %d, want 2", len(chapters))
	}
	if !strings.Contains(chapters[1], `data-toc="1"`) {
		t.Fatalf("second chapter missing toc anchor: %q", chapters[1])
	}
	long := strings.Repeat("字", 70000)
	chunks := TXTChapters(long, nil)
	if len(chunks) != 4 { // 20000×3 + 10000
		t.Fatalf("long text chunks = %d, want 4", len(chunks))
	}
	total := 0
	for _, c := range chunks {
		total += len([]rune(c))
	}
	if total != 70000 {
		t.Fatalf("chunked text length = %d, want 70000", total)
	}
}
