package gateway

import (
	"context"
	"fmt"
)

// GetProject returns a project of id projectId, error otherwise
func (g *Gateway) SendErrors(ctx context.Context, err error) error {
	fmt.Println("hey")
	return nil
}
