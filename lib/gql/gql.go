package gql

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

func AsGQL(ctx context.Context, req interface{}) (*string, error) {
	mp := make(map[string]interface{})
	bytes, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	err = json.Unmarshal(bytes, &mp)
	if err != nil {
		return nil, err
	}
	fields := []string{}
	for k, i := range mp {
		// GQL Selection
		switch i.(type) {
		case bool:
			// GQL Selection
			fields = append(fields, k)
		case map[string]interface{}:
			// Nested GQL/Struct
			nested, err := AsGQL(ctx, i)
			if err != nil {
				return nil, err
			}
			fields = append(fields, fmt.Sprintf("%s {\n%s\n}", k, *nested))
		default:
			return nil, errors.New("Unsupported Type! Cannot generate GQL")
		}
	}
	q := strings.Join(fields, "\n")
	return &q, nil
}
