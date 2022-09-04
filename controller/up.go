package controller

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/railwayapp/cli/entity"
	gitignore "github.com/railwayapp/cli/gateway"
)

var validIgnoreFile = map[string]bool{
	".gitignore":     true,
	".railwayignore": true,
}

var skipDirs = []string{
	".git",
	"node_modules",
}

type ignoreFile struct {
	prefix string
	ignore *gitignore.GitIgnore
}

func scanIgnoreFiles(src string) ([]ignoreFile, error) {
	ignoreFiles := []ignoreFile{}

	if err := filepath.WalkDir(src, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		if d.IsDir() {
			// no sense scanning for ignore files in skipped dirs
			for _, s := range skipDirs {
				if filepath.Base(path) == s {
					return filepath.SkipDir
				}
			}

			return nil
		}

		fname := filepath.Base(path)
		if validIgnoreFile[fname] {
			igf, err := gitignore.CompileIgnoreFile(path)
			if err != nil {
				return err
			}

			prefix := filepath.Dir(path)
			if prefix == "." {
				prefix = "" // Handle root dir properly.
			}

			ignoreFiles = append(ignoreFiles, ignoreFile{
				prefix: prefix,
				ignore: igf,
			})
		}

		return nil
	}); err != nil {
		return nil, err
	}

	return ignoreFiles, nil
}

func compress(src string, buf io.Writer) error {
	// tar > gzip > buf
	zr := gzip.NewWriter(buf)
	tw := tar.NewWriter(zr)

	// find all ignore files, including those in subdirs
	ignoreFiles, err := scanIgnoreFiles(src)
	if err != nil {
		return err
	}

	// walk through every file in the folder
	err = filepath.WalkDir(src, func(absoluteFile string, de os.DirEntry, passedErr error) error {
		relativeFile, err := filepath.Rel(src, absoluteFile)
		if err != nil {
			return err
		}

		if passedErr != nil {
			return err
		}
		if de.IsDir() {
			// skip directories if we can (for perf)
			// e.g., want to avoid walking node_modules dir
			for _, s := range skipDirs {
				if filepath.Base(relativeFile) == s {
					return filepath.SkipDir
				}
			}

			return nil
		}

		for _, igf := range ignoreFiles {
			if strings.HasPrefix(absoluteFile, igf.prefix) { // if ignore file applicable
				trimmed := strings.TrimPrefix(absoluteFile, igf.prefix)
				if igf.ignore.MatchesPath(trimmed) {
					return nil
				}
			}
		}

		// follow symlinks by default
		ln, err := filepath.EvalSymlinks(absoluteFile)
		if err != nil {
			return err
		}
		// get info about the file the link points at
		fi, err := os.Lstat(ln)
		if err != nil {
			return err
		}

		// read file into a buffer to prevent tar overwrites
		data := bytes.NewBuffer(nil)
		f, err := os.Open(ln)
		if err != nil {
			return err
		}
		_, err = io.Copy(data, f)
		if err != nil {
			return err
		}

		// close the file to avoid hitting fd limit
		if err := f.Close(); err != nil {
			return err
		}

		// generate tar headers
		header, err := tar.FileInfoHeader(fi, ln)
		if err != nil {
			return err
		}

		// must provide real name
		// (see https://golang.org/src/archive/tar/common.go?#L626)
		header.Name = filepath.ToSlash(relativeFile)
		// size when we first observed the file
		header.Size = int64(data.Len())

		// write header
		if err := tw.WriteHeader(header); err != nil {
			return err
		}
		// not a dir, write file content
		if _, err := io.Copy(tw, data); err != nil {
			return err
		}
		return err
	})

	if err != nil {
		return err
	}

	// produce tar
	if err := tw.Close(); err != nil {
		return err
	}
	// produce gzip
	if err := zr.Close(); err != nil {
		return err
	}
	return nil
}

func (c *Controller) Upload(
	ctx context.Context,
	req *entity.UploadRequest,
) (*entity.UpResponse, error) {
	var buf bytes.Buffer

	if err := compress(req.RootDir, &buf); err != nil {
		return nil, err
	}

	return c.gtwy.Up(ctx, &entity.UpRequest{
		Data:          buf,
		ProjectID:     req.ProjectID,
		EnvironmentID: req.EnvironmentID,
		ServiceID:     req.ServiceID,
		GitInfo:       req.GitInfo,
	})
}

func (c *Controller) GetFullUrlFromStaticUrl(staticUrl string) string {
	return fmt.Sprintf("https://%s", staticUrl)
}
