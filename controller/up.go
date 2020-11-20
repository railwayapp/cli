package controller

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/railwayapp/cli/entity"
	gitignore "github.com/railwayapp/cli/gateway"
)

func compress(src string, buf io.Writer) error {
	// tar > gzip > buf
	zr := gzip.NewWriter(buf)
	tw := tar.NewWriter(zr)

	ignore, err := gitignore.CompileIgnoreFile(".gitignore")

	if err != nil {
		return err
	}

	// walk through every file in the folder
	filepath.Walk(src, func(file string, fi os.FileInfo, err error) error {
		if fi.IsDir() {
			return nil
		}

		if strings.HasPrefix(file, ".git") {
			return nil
		}

		if ignore.MatchesPath(file) {
			return nil
		}

		// generate tar header
		header, err := tar.FileInfoHeader(fi, file)
		if err != nil {
			return err
		}

		if err != nil {
			return err
		}
		// must provide real name
		// (see https://golang.org/src/archive/tar/common.go?#L626)
		header.Name = filepath.ToSlash(file)

		// write header
		if err := tw.WriteHeader(header); err != nil {
			return err
		}
		// if not a dir, write file content
		if !fi.IsDir() {
			data, err := os.Open(file)
			if err != nil {
				return err
			}
			if _, err := io.Copy(tw, data); err != nil {
				return err
			}
		}

		return nil
	})

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

func (c *Controller) Up(ctx context.Context) (string, error) {
	projectID, err := c.cfg.GetProject()
	if err != nil {
		return "", err
	}
	environmentID, err := c.cfg.GetEnvironment()
	if err != nil {
		return "", err
	}
	var buf bytes.Buffer
	if err := compress(".", &buf); err != nil {
		return "", err
	}

	res, err := c.gtwy.Up(ctx, &entity.UpRequest{
		Data:          buf,
		ProjectID:     projectID,
		EnvironmentID: environmentID,
	})
	if err != nil {
		return "", err
	}
	return res.URL, nil
}
