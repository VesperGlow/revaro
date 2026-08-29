package dataplane

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/exec"
	"sync"
	"syscall"
	"time"
)

type Process struct {
	cmd   *exec.Cmd
	token string
	addr  string
	done  chan error
	once  sync.Once
	log   *slog.Logger
}

func Start(ctx context.Context, binary, addr string, logger *slog.Logger) (*Process, error) {
	host, _, err := net.SplitHostPort(addr)
	if err != nil || net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		return nil, errors.New("DATA_PLANE_ADDR must be a numeric loopback host:port")
	}
	random := make([]byte, 32)
	if _, err := rand.Read(random); err != nil {
		return nil, fmt.Errorf("generate data-plane token: %w", err)
	}
	token := hex.EncodeToString(random)
	cmd := exec.Command(binary)
	cmd.Env = append(os.Environ(), "REVARO_DATA_PLANE_ADDR="+addr, "REVARO_DATA_PLANE_TOKEN="+token)
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start Rust data plane: %w", err)
	}
	p := &Process{cmd: cmd, token: token, addr: addr, done: make(chan error, 1), log: logger}
	go func() { p.done <- cmd.Wait(); close(p.done) }()
	readyCtx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	if err := p.waitReady(readyCtx); err != nil {
		p.stop()
		return nil, err
	}
	return p, nil
}

func (p *Process) Token() string      { return p.token }
func (p *Process) Addr() string       { return p.addr }
func (p *Process) Done() <-chan error { return p.done }

func (p *Process) waitReady(ctx context.Context) error {
	client := &http.Client{Timeout: time.Second}
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	for {
		req, _ := http.NewRequestWithContext(ctx, http.MethodGet, "http://"+p.addr+"/v1/health", nil)
		req.Header.Set("Authorization", "Bearer "+p.token)
		if resp, err := client.Do(req); err == nil {
			var health struct {
				Status   string
				Protocol int
			}
			decodeErr := json.NewDecoder(io.LimitReader(resp.Body, 4096)).Decode(&health)
			resp.Body.Close()
			if resp.StatusCode == http.StatusOK && decodeErr == nil && health.Status == "ok" && health.Protocol == 1 {
				return nil
			}
		}
		select {
		case err := <-p.done:
			return fmt.Errorf("Rust data plane exited before ready: %w", err)
		case <-ctx.Done():
			return fmt.Errorf("Rust data plane readiness: %w", ctx.Err())
		case <-ticker.C:
		}
	}
}

func (p *Process) stop() {
	p.once.Do(func() {
		if p.cmd.Process == nil {
			return
		}
		_ = syscall.Kill(-p.cmd.Process.Pid, syscall.SIGTERM)
		select {
		case <-p.done:
		case <-time.After(10 * time.Second):
			_ = syscall.Kill(-p.cmd.Process.Pid, syscall.SIGKILL)
			<-p.done
		}
	})
}

func (p *Process) Close() { p.stop() }
