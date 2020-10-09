package github

import (
	"context"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"net/http"

	gql "github.com/machinebox/graphql"
	entity "github.com/railwayapp/cli/entity"
)

const (
	TEMPLATE_REPO_URL = "https://api.github.com/repos/railwayapp/examples/contents"
)

func (g *Gateway) GetFilesInRepository(ctx context.Context, req *entity.GithubFilesRequest) ([]*entity.GithubFile, error) {
	res, err := http.Get(fmt.Sprintf("%s/%s", TEMPLATE_REPO_URL, req.Path))
	if err != nil {
		return nil, err
	}
	body, err := ioutil.ReadAll(res.Body)
	if err != nil {
		return nil, err
	}
	files := []*entity.GithubFile{}
	err = json.Unmarshal(body, &files)
	if err != nil {
		return nil, err
	}
	return files, nil
}

func (g *Gateway) GetFilesInRepositoryGQL(ctx context.Context, req *entity.GithubFilesRequest) ([]*entity.GithubFile, error) {
	gqlReq := gql.NewRequest(`
		query($repoName: String!, $repoOwner: String!, $repoPointer: String!) {
			repository(name: $repoName, owner: $repoOwner) {
				object(expression: $repoPointer) {
					... on Tree {
						entries {
							name
							type
							mode
						}
					}
				}
			}
		}
	`)
	fmt.Println(gqlReq, req)
	gqlReq.Var("repoName", req.RepoName)
	gqlReq.Var("repoOwner", req.RepoOwner)
	gqlReq.Var("repoPointer", fmt.Sprintf("master:%s", req.Path))

	var resp struct {
		Data struct {
			Repository struct {
				Object struct {
					Entries []*entity.GithubFile
				}
			} `json:"repository"`
		} `json:"data"`
	}

	if err := g.gqlClient.Run(ctx, gqlReq, &resp); err != nil {
		return nil, err
	}
	return resp.Data.Repository.Object.Entries, nil
}
