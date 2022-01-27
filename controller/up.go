package controller

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"fmt"
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
	err = filepath.WalkDir(src, func(file string, de os.DirEntry, passedErr error) error {
		if passedErr != nil {
			return err
		}
		if de.IsDir() {
			return nil
		}

		fi, err := de.Info()
		if err != nil {
			return err
		}

		if fi.Mode()&os.ModeSymlink == os.ModeSymlink {
			// Skip symlinks
			// TODO: Follow em and detect cycles
			return nil
		}

		if strings.Match(file, ".git") || strings.Match(file, "node_modules") {
			return nil
		}

		if ignore.MatchesPath(file) {
			return nil
		}

		// read file into a buffer to prevent tar overwrites
		data := bytes.NewBuffer(nil)
		f, err := os.Open(file)
		if err != nil {
			return err
		}
		_, err = io.Copy(data, f)
		if err != nil {
			return err
		}

		// generate tar headers
		header, err := tar.FileInfoHeader(fi, file)
		if err != nil {
			return err
		}

		// must provide real name
		// (see https://golang.org/src/archive/tar/common.go?#L626)
		header.Name = filepath.ToSlash(file)

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

func (c *Controller) Upload(ctx context.Context, req *entity.UploadRequest) (*entity.UpResponse, error) {
	var buf bytes.Buffer

	if err := compress(req.RootDir, &buf); err != nil {
		return nil, err
	}

	res, err := c.gtwy.Up(ctx, &entity.UpRequest{
		Data:          buf,
		ProjectID:     req.ProjectID,
		EnvironmentID: req.EnvironmentID,
		ServiceID:     req.ServiceID,
	})
	if err != nil {
		return nil, err
	}

	return res, nil
}

func (c *Controller) GetFullUrlFromStaticUrl(staticUrl string) string {
	return fmt.Sprintf("https://%s", staticUrl)
}
