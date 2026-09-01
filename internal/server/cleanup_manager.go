package server

import (
	"context"
	"log/slog"
	"sync"
	"time"
)

type cleanupJob struct {
	name              string
	interval, timeout time.Duration
	next              time.Time
	failures          int
	run               func(context.Context) error
}
type CleanupManager struct {
	mu        sync.Mutex
	jobs      map[string]*cleanupJob
	wake      chan struct{}
	ctx       context.Context
	cancel    context.CancelFunc
	wg        sync.WaitGroup
	log       *slog.Logger
	resources *ResourceGovernor
}

func newCleanupManager(log *slog.Logger, resources *ResourceGovernor) *CleanupManager {
	ctx, cancel := context.WithCancel(context.Background())
	return &CleanupManager{jobs: map[string]*cleanupJob{}, wake: make(chan struct{}, 1), ctx: ctx, cancel: cancel, log: log, resources: resources}
}
func (m *CleanupManager) Register(name string, interval, timeout time.Duration, runNow bool, run func(context.Context) error) {
	m.mu.Lock()
	next := time.Now().Add(interval)
	if runNow {
		next = time.Now()
	}
	m.jobs[name] = &cleanupJob{name: name, interval: interval, timeout: timeout, next: next, run: run}
	m.mu.Unlock()
	select {
	case m.wake <- struct{}{}:
	default:
	}
}
func (m *CleanupManager) Start() { m.wg.Add(1); go m.loop() }
func (m *CleanupManager) Close() { m.cancel(); m.wg.Wait() }
func (m *CleanupManager) loop() {
	defer m.wg.Done()
	timer := time.NewTimer(0)
	defer timer.Stop()
	for {
		select {
		case <-m.ctx.Done():
			return
		case <-m.wake:
		case <-timer.C:
		}
		now := time.Now()
		m.mu.Lock()
		due := []*cleanupJob{}
		next := now.Add(time.Hour)
		for _, job := range m.jobs {
			if !job.next.After(now) {
				due = append(due, job)
				job.next = now.Add(job.interval)
			}
			if job.next.Before(next) {
				next = job.next
			}
		}
		m.mu.Unlock()
		for _, job := range due {
			m.run(job)
		}
		delay := time.Until(next)
		if delay < time.Second {
			delay = time.Second
		}
		timer.Reset(delay)
	}
}
func (m *CleanupManager) run(job *cleanupJob) {
	ctx, cancel := context.WithTimeout(m.ctx, job.timeout)
	defer cancel()
	release, err := m.resources.IO(ctx)
	if err == nil {
		err = job.run(ctx)
		release()
	}
	m.mu.Lock()
	if err != nil {
		job.failures++
		backoff := time.Duration(1<<min(job.failures, 6)) * time.Minute
		if retry := time.Now().Add(backoff); retry.Before(job.next) {
			job.next = retry
		}
	} else {
		job.failures = 0
	}
	m.mu.Unlock()
	if err != nil {
		m.log.Warn("cleanup pass failed", "job", job.name, "retryable", true, "error", err)
	}
}
