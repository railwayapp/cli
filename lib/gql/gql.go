package gql

import (
	"context"
	"encoding/json"
	"strings"
)

func AsGQL(ctx context.Context, req interface{}) (*string, error) {
	// Assume object is a flat keystruct
	mp := make(map[string]bool)
	bytes, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	err = json.Unmarshal(bytes, &mp)
	if err != nil {
		return nil, err
	}
	fields := []string{}
	for k, v := range mp {
		if v {
			fields = append(fields, k)
		}
	}
	q := strings.Join(fields, "\n")
	return &q, nil
}
