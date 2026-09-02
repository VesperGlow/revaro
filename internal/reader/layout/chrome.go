package layout

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
)

// Chrome 探测：分页器需要本机一个可用的 Chromium/Chrome 可执行文件。
// 查找顺序：REVARO_CHROME_BIN → PATH 常见名字 → Playwright/Puppeteer
// 的本地缓存目录（开发机与 CI 常见位置）。结果按版本缓存。

var chromeMu sync.Mutex
var chromeCache *engineInfo

// EngineInfo 报告布局引擎可用性，供 capabilities 端点与日志使用。
type EngineInfo struct {
	Available bool   `json:"available"`
	Engine    string `json:"engine,omitempty"`
	Version   string `json:"version,omitempty"`
	Path      string `json:"path,omitempty"`
	Reason    string `json:"reason,omitempty"`
}

type engineInfo struct {
	path    string
	version string
}

// DetectEngine 返回当前进程的布局引擎状态；结果缓存，进程内只探测一次。
func DetectEngine() EngineInfo {
	chromeMu.Lock()
	defer chromeMu.Unlock()
	if chromeCache == nil {
		path, err := findChrome()
		if err != nil {
			chromeCache = &engineInfo{}
		} else {
			version, verErr := chromeVersion(path)
			if verErr != nil {
				version = ""
			}
			chromeCache = &engineInfo{path: path, version: version}
		}
	}
	if chromeCache.path == "" {
		return EngineInfo{Available: false, Reason: "chromium 可执行文件未找到（可用 REVARO_CHROME_BIN 指定）"}
	}
	return EngineInfo{Available: true, Engine: "chromium", Version: chromeCache.version, Path: chromeCache.path}
}

func findChrome() (string, error) {
	candidates := []string{}
	if bin := strings.TrimSpace(os.Getenv("REVARO_CHROME_BIN")); bin != "" {
		candidates = append(candidates, bin)
	}
	for _, name := range []string{
		"chromium", "chromium-browser", "google-chrome", "google-chrome-stable",
		"chrome", "headless_shell", "chrome-headless-shell",
	} {
		if p, err := exec.LookPath(name); err == nil {
			candidates = append(candidates, p)
		}
	}
	candidates = append(candidates, cacheCandidates()...)
	for _, p := range candidates {
		info, err := os.Stat(p)
		if err != nil || info.IsDir() {
			continue
		}
		if err := checkChromeRun(p); err == nil {
			return p, nil
		}
	}
	return "", fmt.Errorf("no usable chromium binary found")
}

// cacheCandidates 枚举 Playwright/Puppeteer 的本地下载目录。
func cacheCandidates() []string {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	var out []string
	if runtime.GOOS == "darwin" {
		out = append(out,
			filepath.Join(home, "Library/Caches/ms-playwright", "*", "chrome-mac*", "Chromium.app", "Contents", "MacOS", "Chromium"),
			filepath.Join(home, ".cache/puppeteer/chrome", "*", "chrome-mac*", "Chromium.app", "Contents", "MacOS", "Chromium"),
		)
	}
	out = append(out,
		filepath.Join(home, ".cache/ms-playwright", "chromium_headless_shell-*", "chrome-linux", "headless_shell"),
		filepath.Join(home, ".cache/ms-playwright", "chromium-*", "chrome-linux", "chrome"),
		filepath.Join(home, ".cache/puppeteer/chrome", "*", "chrome-linux*", "*"),
		filepath.Join(home, ".cache/chrome", "chrome-headless-shell", "*", "chrome-headless-shell"),
	)
	var matches []string
	for _, pattern := range out {
		got, err := filepath.Glob(pattern)
		if err != nil {
			continue
		}
		matches = append(matches, got...)
	}
	sort.Strings(matches)
	return matches
}

// checkChromeRun 快速确认候选文件是可执行且能输出版本号的浏览器。
func checkChromeRun(path string) error {
	cmd := exec.Command(path, "--version")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return err
	}
	if !strings.Contains(string(output), "Chrom") && !strings.Contains(string(output), "Chrome") {
		return fmt.Errorf("not a chromium binary: %s", strings.TrimSpace(string(output)))
	}
	return nil
}

func chromeVersion(path string) (string, error) {
	cmd := exec.Command(path, "--version")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return "", err
	}
	fields := strings.Fields(string(output))
	if len(fields) < 2 {
		return strings.TrimSpace(string(output)), nil
	}
	return fields[len(fields)-1], nil
}
