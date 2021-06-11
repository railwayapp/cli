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
	err = filepath.Walk(src, func(file string, fi os.FileInfo, passedErr error) error {
		if passedErr != nil {
			return err
		}
		if fi.IsDir() {
			return nil
		}

		if strings.HasPrefix(file, ".git") || strings.HasPrefix(file, "node_modules") {
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

func (c *Controller) Up(ctx context.Context) (string, error) {
	projectConfig, err := c.GetProjectConfigs(ctx)
	if err != nil {
		return "", err
	}

	var buf bytes.Buffer
	if err := compress(".", &buf); err != nil {
		return "", err
	}

	res, err := c.gtwy.Up(ctx, &entity.UpRequest{
		Data:          buf,
		ProjectID:     projectConfig.Project,
		EnvironmentID: projectConfig.Environment,
	})
	if err != nil {
		return "", err
	}
	return res.URL, nil
}
