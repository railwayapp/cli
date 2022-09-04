package git

type CommitInfo struct {
	Hash    string
	Message string
	Author  string
}
type GitMetadata struct {
	IsRepo          bool
	RepoName        string
	Branch          string
	Commit          CommitInfo
	HasLocalChanges bool
}
