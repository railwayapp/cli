package entity

/* GithubFilesRequest defines the format for GQL queries againt the GH API
   - Reponame: The repository name
   - RepoOwner: The repo owner
   - RepoPointer: A branch:file pairing* (e.g "master:README.md")
   * For root, use "master:"
*/
type GithubFilesRequest struct {
	RepoName  string
	RepoOwner string
	Path      string
}

type GithubFile struct {
	Name string
	Type string
	Mode int
}
