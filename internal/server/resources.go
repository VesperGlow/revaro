package server

import "context"

// ResourceGovernor composes with the existing per-feature limits and prevents
// unrelated workloads from consuming all CPU or disk/S3 IO at once.
type ResourceGovernor struct {
	cpu chan struct{}
	io  chan struct{}
}

func newResourceGovernor() *ResourceGovernor {
	return &ResourceGovernor{cpu: make(chan struct{}, 1), io: make(chan struct{}, 3)}
}

func acquireResource(ctx context.Context, slot chan struct{}) (func(), error) {
	select {
	case slot <- struct{}{}:
		return func() { <-slot }, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func (g *ResourceGovernor) Heavy(ctx context.Context) (func(), error) {
	releaseCPU, err := acquireResource(ctx, g.cpu)
	if err != nil {
		return nil, err
	}
	releaseIO, err := acquireResource(ctx, g.io)
	if err != nil {
		releaseCPU()
		return nil, err
	}
	return func() { releaseIO(); releaseCPU() }, nil
}
func (g *ResourceGovernor) IO(ctx context.Context) (func(), error) { return acquireResource(ctx, g.io) }
