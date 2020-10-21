package gateway

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
)

// GetProject returns a project of id projectId, error otherwise
func (g *Gateway) SendPanic(ctx context.Context, i interface{}) error {
	url := fmt.Sprintf("%s/cli/panic", GetHost())
	jsonValue, _ := json.Marshal(i)
	req, _ := http.NewRequest("POST", url, bytes.NewBuffer(jsonValue))
	fmt.Println("", req.Body)
	req.Header.Set("Content-Type", "application/json")
	res, _ := http.DefaultClient.Do(req)

	defer res.Body.Close()
	// body, _ := ioutil.ReadAll(res.Body)
	fmt.Println("sent it", res)
	return nil
}
