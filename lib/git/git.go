package git

import (
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

/**
 * Parses url with the given regular expression and returns the
 * group values defined in the expression.
 * https://stackoverflow.com/a/39635221
 */
func getParams(regEx, test string) (paramsMap map[string]string) {

	var compRegEx = regexp.MustCompile(regEx)
	match := compRegEx.FindStringSubmatch(test)

	paramsMap = make(map[string]string)
	for i, name := range compRegEx.SubexpNames() {
		if i > 0 && i <= len(match) {
			paramsMap[name] = match[i]
		}
	}
	return paramsMap
}

func execGit(path string, cmd ...string) ([]byte, error) {
	args := []string{}
	args = append(args, "-C", path)
	args = append(args, cmd...)
	gitCmd := exec.Command("git", args...)
	return gitCmd.Output()
}

func IsRepo(path string) bool {
	_, err := execGit(path, "status")
	return err == nil
}

func RepoName(path string) (string, error) {
	remoteRegex := `(?:(?:.*?\@.*?\..*?\:)|(?:https\:\/\/.*?\..*?\/))(?P<User>.*?)\/(?P<Repo>.*?)\.git`
	remotes, err := execGit(path, "remote")
	useRemotes := true
	if err != nil || strings.Trim(string(remotes), " \n") == "" {
		useRemotes = false
	}
	if useRemotes {
		for _, remote := range strings.Split(string(remotes), "\n") {
			if remote == "origin" {
				remoteUrlBytes, err := execGit(path, "remote", "get-url", remote)
				if err != nil {
					break
				}
				remoteUrl := strings.Trim(string(remoteUrlBytes), " \n")
				if !strings.HasSuffix(string(remoteUrl), ".git") {
					remoteUrl += ".git"
				}
				match := getParams(remoteRegex, remoteUrl)
				return match["User"] + "/" + match["Repo"], nil
			}
		}
	}
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	base := filepath.Base(abs)
	return base, nil
}

func GetBranch(path string) (string, error) {
	branch, err := execGit(path, "branch", "--show-current")
	return string(branch), err
}

func GetCommit(path string) (CommitInfo, error) {
	hash, err := execGit(path, "log", "-1", "--format=format:%h")
	if err != nil {
		return CommitInfo{}, err
	}
	message, err := execGit(path, "log", "-1", `--format=format:%s%n%b`)
	if err != nil {
		return CommitInfo{}, err
	}
	author, err := execGit(path, "log", "-1", "--format=format:%an <%ae>")
	if err != nil {
		return CommitInfo{}, err
	}
	return CommitInfo{
		Hash:    string(hash),
		Message: string(message),
		Author:  string(author),
	}, nil
}

func GetAllMetadata(path string) (GitMetadata, error) {
	name, err := RepoName(path)
	if err != nil {
		return GitMetadata{IsRepo: false}, err
	}

	branch, err := GetBranch(path)
	if err != nil {
		return GitMetadata{IsRepo: false}, err
	}

	commit, err := GetCommit(path)
	if err != nil {
		return GitMetadata{IsRepo: false}, err
	}

	return GitMetadata{
		IsRepo:   IsRepo(path),
		RepoName: name,
		Branch:   branch,
		Commit:   commit,
	}, nil
}
